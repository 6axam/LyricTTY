use crate::{
    alignment::AlignmentHandle,
    artwork::{Artwork, ArtworkProvider},
    config::Config,
    content::LyricsContent,
    playback::Track,
    providers::{load_local, LyricsCache, LyricsDocument, LyricsService},
};
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
pub enum AssetEvent {
    PrefetchStatus { message: String },
    AlignmentReady { key: String, prepared: bool },
    AlignmentStatus { key: String, message: String },
    Lyrics { key: String, content: LyricsContent },
    Artwork { key: String, artwork: Artwork },
}
struct Request {
    generation: u64,
    key: String,
    track: Track,
    local: Option<PathBuf>,
    demo: bool,
}
pub struct AssetService {
    alignment: AlignmentHandle,
    requests: Sender<Request>,
    pub events: Receiver<AssetEvent>,
    prefetch_requests: Sender<(u64, Vec<Track>)>,
    prefetch_epoch: Arc<AtomicU64>,
    prefetch_stop: Arc<AtomicBool>,
    prefetch_worker: Option<thread::JoinHandle<()>>,
}
impl AssetService {
    pub fn start(config: Config, cache: PathBuf) -> Self {
        let (requests, rx) = mpsc::channel::<Request>();
        let (tx, events) = mpsc::channel();
        let alignment = AlignmentHandle::start_with_threads(
            cache.join("stable-words-v1"),
            tx.clone(),
            usize::from(config.alignment_threads),
        );
        let align_worker = alignment.clone();
        let (prefetch_requests, prefetch_rx) = mpsc::channel::<(u64, Vec<Track>)>();
        let prefetch_epoch = Arc::new(AtomicU64::new(0));
        let prefetch_stop = Arc::new(AtomicBool::new(false));
        let worker_epoch = prefetch_epoch.clone();
        let worker_stop = prefetch_stop.clone();
        let warm_align = alignment.clone();
        let warm_events = tx.clone();
        let warm_cache = cache.join("lyrics");
        let warm_enabled = config.audio_alignment && config.online_lyrics;
        let prefetch_worker = thread::spawn(move || {
            let mut metadata_cache = std::collections::HashMap::<String, Track>::new();
            let lyrics = if warm_enabled {
                LyricsService::new(Some(LyricsCache::new(warm_cache))).ok()
            } else {
                None
            };
            while !worker_stop.load(Ordering::Relaxed) {
                let mut batch = match prefetch_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(batch) => batch,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                while let Ok(next) = prefetch_rx.try_recv() {
                    batch = next;
                }
                let (epoch, tracks) = batch;
                for track in tracks.into_iter().take(3) {
                    if worker_stop.load(Ordering::Relaxed)
                        || worker_epoch.load(Ordering::Relaxed) != epoch
                    {
                        break;
                    }
                    let Some(service) = &lyrics else {
                        continue;
                    };
                    let metadata_key = serde_json::to_string(&(
                        &track.url,
                        &track.title,
                        &track.artist,
                        &track.album,
                        track.duration.round() as i64,
                    ))
                    .unwrap_or_default();
                    let resolved = if let Some(known) = metadata_cache.get(&metadata_key) {
                        Ok(known.clone())
                    } else {
                        resolve_upcoming(track, &worker_stop, &worker_epoch, epoch)
                    };
                    let Ok(track) = resolved else {
                        if !worker_stop.load(Ordering::Relaxed)
                            && worker_epoch.load(Ordering::Relaxed) == epoch
                        {
                            let _ = warm_events.send(AssetEvent::PrefetchStatus {
                                message: "Не удалось прочитать данные следующего трека".into(),
                            });
                        }
                        continue;
                    };
                    if metadata_cache.len() >= 64 {
                        metadata_cache.clear();
                    }
                    metadata_cache.insert(metadata_key, track.clone());
                    let Ok(document) = service.lookup(&crate::metadata::lyrics_query(&track))
                    else {
                        continue;
                    };
                    if worker_stop.load(Ordering::Relaxed)
                        || worker_epoch.load(Ordering::Relaxed) != epoch
                    {
                        break;
                    }
                    if document.lines.is_empty() || document.instrumental {
                        continue;
                    }
                    let content = LyricsContent {
                        lines: document.lines,
                        plain: document.plain,
                        quality: document.source,
                        instrumental: false,
                    };
                    let _ = warm_events.send(AssetEvent::PrefetchStatus {
                        message: format!("В очереди на подготовку: {}", track.title),
                    });
                    warm_align.prefetch(track, content, epoch);
                }
            }
        });
        // Dedicated workers let a slow lyrics server coexist with a fast cover.
        let (art_tx, art_rx) = mpsc::channel::<(String, String)>();
        let cover_tx = tx.clone();
        let art_cache = cache.join("artwork");
        thread::spawn(move || {
            let provider = ArtworkProvider::new(art_cache).ok();
            while let Ok(mut request) = art_rx.recv() {
                while let Ok(new) = art_rx.try_recv() {
                    request = new;
                }
                if let Some(provider) = &provider {
                    if let Ok(artwork) = provider.load(&request.1) {
                        if cover_tx
                            .send(AssetEvent::Artwork {
                                key: request.0,
                                artwork,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        thread::spawn(move || {
            let lyrics = if config.online_lyrics {
                LyricsService::new(Some(LyricsCache::new(cache.join("lyrics")))).ok()
            } else {
                None
            };
            while let Ok(mut r) = rx.recv() {
                while let Ok(new) = rx.try_recv() {
                    r = new;
                }
                if r.demo {
                    if let Ok(artwork) =
                        Artwork::from_bytes(include_bytes!("../assets/demo-cover.png"))
                    {
                        let _ = tx.send(AssetEvent::Artwork {
                            key: r.key.clone(),
                            artwork,
                        });
                    }
                }
                if config.show_artwork && !config.low_data {
                    if let Some(url) = &r.track.artwork_url {
                        let _ = art_tx.send((r.key.clone(), url.clone()));
                    }
                }
                let result = if let Some(path) = r.local {
                    load_local(path)
                } else if r.demo {
                    let lines = crate::lyrics::parse_lrc(include_str!("../examples/demo.lrc"));
                    Ok(LyricsDocument {
                        lines,
                        quality: crate::providers::LyricsQuality::Word,
                        source: "Authored demo fixture".into(),
                        ..Default::default()
                    })
                } else if let Some(service) = &lyrics {
                    service.lookup(&crate::metadata::lyrics_query(&r.track))
                } else {
                    Ok(LyricsDocument::default())
                };
                let content = match result {
                    Ok(document) => {
                        let quality = format!(
                            "{} · {}{}",
                            document.quality_label(),
                            document.source,
                            if document.cached { " · cached" } else { "" }
                        );
                        LyricsContent {
                            lines: document.lines,
                            plain: document.plain,
                            quality,
                            instrumental: document.instrumental,
                        }
                    }
                    Err(_) => LyricsContent {
                        quality: "Lyrics unavailable · playback continues".into(),
                        ..Default::default()
                    },
                };
                let align_content = content.clone();
                if tx
                    .send(AssetEvent::Lyrics {
                        key: r.key.clone(),
                        content,
                    })
                    .is_err()
                {
                    break;
                }
                if config.audio_alignment && !r.demo {
                    align_worker.request(r.key, r.track, align_content, r.generation);
                }
            }
        });
        Self {
            requests,
            events,
            alignment,
            prefetch_requests,
            prefetch_epoch,
            prefetch_stop,
            prefetch_worker: Some(prefetch_worker),
        }
    }
    pub fn prefetch(&self, tracks: Vec<Track>) {
        let epoch = self.alignment.begin_prefetch();
        self.prefetch_epoch.store(epoch, Ordering::Relaxed);
        let _ = self.prefetch_requests.send((epoch, tracks));
    }
    pub fn cancel(&self) {
        self.alignment.invalidate();
        self.prefetch(Vec::new());
    }
    pub fn request(&self, key: String, track: Track, local: Option<PathBuf>, demo: bool) {
        let _ = self.requests.send(Request {
            generation: self.alignment.invalidate(),
            key,
            track,
            local,
            demo,
        });
    }
}
impl Drop for AssetService {
    fn drop(&mut self) {
        self.prefetch_stop.store(true, Ordering::Relaxed);
        self.alignment.stop();
        if let Some(worker) = self.prefetch_worker.take() {
            let _ = worker.join();
        }
    }
}

fn resolve_upcoming(
    track: Track,
    stop: &AtomicBool,
    generation: &AtomicU64,
    epoch: u64,
) -> anyhow::Result<Track> {
    if !track.title.is_empty() && !track.artist.is_empty() && track.duration > 0.0 {
        return Ok(track);
    }
    let root = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lyrictty/alignment");
    let temp = tempfile::tempdir()?;
    let helper = temp.path().join("resolve.py");
    let input = temp.path().join("request.json");
    let output = temp.path().join("result.json");
    fs::write(&helper, include_str!("../scripts/resolve_track.py"))?;
    fs::write(&input, serde_json::to_vec(&track)?)?;
    let mut command = Command::new(root.join("venv/bin/python"));
    command
        .arg(helper)
        .arg("--request")
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::from(fs::File::create(&output)?))
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = MetadataChild(command.spawn()?);
    let started = Instant::now();
    loop {
        anyhow::ensure!(
            !stop.load(Ordering::Relaxed) && generation.load(Ordering::Relaxed) == epoch,
            "prefetch cancelled"
        );
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(30),
            "metadata timed out"
        );
        if let Some(status) = child.0.try_wait()? {
            anyhow::ensure!(status.success(), "metadata unavailable");
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
    let bytes = fs::read(output)?;
    anyhow::ensure!(bytes.len() <= 128 * 1024, "metadata too large");
    Ok(serde_json::from_slice(&bytes)?)
}
struct MetadataChild(Child);
impl Drop for MetadataChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .args(["-TERM", "--", &format!("-{}", self.0.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
