//! Playback-output recognition runs in one disposable, low-priority process.
use crate::{
    assets::AssetEvent,
    content::LyricsContent,
    lyrics::LyricLine,
    playback::{PlaybackSnapshot, PlaybackStatus, Track},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct AlignmentHandle {
    requests: Sender<Request>,
    playback: Arc<Mutex<Playback>>,
    generation: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    worker: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}
struct Request {
    key: String,
    track: Track,
    content: LyricsContent,
    generation: u64,
}
struct Playback {
    key: String,
    position: f64,
    rate: f64,
    playing: bool,
    updated: Instant,
}
impl Default for Playback {
    fn default() -> Self {
        Self {
            key: String::new(),
            position: 0.0,
            rate: 1.0,
            playing: false,
            updated: Instant::now(),
        }
    }
}
#[derive(Serialize)]
struct Clock {
    position: f64,
    playing: bool,
    rate: f64,
}
#[derive(Serialize, Deserialize)]
struct Cached {
    lines: Vec<LyricLine>,
}
#[derive(Deserialize)]
struct Update {
    line: usize,
    words: Vec<crate::lyrics::LyricWord>,
}

impl AlignmentHandle {
    pub fn start(cache: PathBuf, events: Sender<AssetEvent>) -> Self {
        let (requests, rx) = mpsc::channel::<Request>();
        let handle = Self {
            requests,
            playback: Arc::new(Mutex::new(Playback::default())),
            generation: Arc::new(AtomicU64::new(0)),
            stopped: Arc::new(AtomicBool::new(false)),
            worker: Arc::new(Mutex::new(None)),
        };
        let worker = handle.clone();
        let task = thread::spawn(move || {
            while !worker.stopped.load(Ordering::Relaxed) {
                let mut request = match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                while let Ok(next) = rx.try_recv() {
                    request = next;
                }
                if worker.cancelled(&request) {
                    continue;
                }
                if let Err(error) = worker.align(&request, &cache, &events) {
                    if !worker.cancelled(&request) {
                        report(&events, &request.key, &format!("Слова по голосу: {error}"));
                    }
                }
            }
        });
        *handle.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        handle
    }
    pub fn invalidate(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.invalidate();
        if let Some(task) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = task.join();
        }
    }
    pub fn request(&self, key: String, track: Track, content: LyricsContent, generation: u64) {
        let _ = self.requests.send(Request {
            key,
            track,
            content,
            generation,
        });
    }
    pub fn playback(&self, key: &str, source: Option<&PlaybackSnapshot>) {
        let mut state = self.playback.lock().unwrap_or_else(|e| e.into_inner());
        *state = Playback {
            key: key.into(),
            position: source.map(|s| s.position_now()).unwrap_or(0.0),
            rate: source.map(|s| s.rate).unwrap_or(1.0),
            playing: source.is_some_and(|s| s.status == PlaybackStatus::Playing),
            updated: Instant::now(),
        };
    }
    fn cancelled(&self, request: &Request) -> bool {
        self.stopped.load(Ordering::Relaxed)
            || self.generation.load(Ordering::Relaxed) != request.generation
    }
    fn clock(&self, key: &str) -> Clock {
        let state = self.playback.lock().unwrap_or_else(|e| e.into_inner());
        let playing = state.key == key && state.playing;
        Clock {
            position: state.position
                + if playing {
                    state.updated.elapsed().as_secs_f64() * state.rate
                } else {
                    0.0
                },
            playing,
            rate: state.rate,
        }
    }
    fn align(
        &self,
        request: &Request,
        cache: &Path,
        events: &Sender<AssetEvent>,
    ) -> anyhow::Result<()> {
        if request.content.lines.is_empty()
            || request.content.instrumental
            || request.content.lines.iter().all(complete)
        {
            return Ok(());
        }
        fs::create_dir_all(cache)?;
        let fingerprint = serde_json::to_vec(&(
            &request.track.artist,
            &request.track.title,
            &request.track.album,
            request.track.duration.round() as i64,
            &request.content.lines,
        ))?;
        let path = cache.join(format!("{:x}.json", Sha256::digest(fingerprint)));
        let mut content = request.content.clone();
        if let Ok(bytes) = fs::read(&path) {
            if bytes.len() <= 4 * 1024 * 1024 {
                if let Ok(cached) = serde_json::from_slice::<Cached>(&bytes) {
                    if cached.lines.len() == content.lines.len() {
                        for (base, saved) in content.lines.iter_mut().zip(cached.lines) {
                            if base.text == saved.text
                                && base.start == saved.start
                                && base.end == saved.end
                                && valid_words(base, &saved.words)
                                && !complete(base)
                            {
                                base.words = saved.words;
                            }
                        }
                        publish(events, &request.key, &content);
                    }
                }
            }
        }
        if content.lines.iter().all(complete) {
            report(events, &request.key, "По словам · сохранённые тайминги");
            return Ok(());
        }
        let root = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("lyrictty/alignment");
        let cyrillic = content
            .lines
            .iter()
            .flat_map(|l| l.text.chars())
            .filter(|c| matches!(*c as u32, 0x0400..=0x04ff))
            .count();
        let model_name = if cyrillic > 3 {
            "vosk-model-small-ru-0.22"
        } else {
            "vosk-model-small-en-us-0.15"
        };
        let python = root.join("venv/bin/python");
        let model = root.join("models").join(model_name);
        anyhow::ensure!(
            python.exists() && model.join("am/final.mdl").exists(),
            "нужна небольшая модель; запусти scripts/setup-alignment.sh {}",
            if cyrillic > 3 { "ru" } else { "en" }
        );
        let temp = tempfile::tempdir()?;
        let helper = temp.path().join("align.py");
        let input = temp.path().join("request.json");
        fs::write(&helper, include_str!("../scripts/align_lyrics.py"))?;
        let initial = self.clock(&request.key);
        fs::write(
            &input,
            serde_json::to_vec(
                &serde_json::json!({"lines":content.lines, "position":initial.position, "playing":initial.playing, "rate":initial.rate}),
            )?,
        )?;
        let log = temp.path().join("error.log");
        let mut command = Command::new(python);
        command
            .args(["-u"])
            .arg(helper)
            .arg("--request")
            .arg(input)
            .arg("--model")
            .arg(model)
            .env("OMP_NUM_THREADS", "1")
            .env("OPENBLAS_NUM_THREADS", "1")
            .env("MKL_NUM_THREADS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(fs::File::create(&log)?));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = Process(command.spawn()?);
        let stdout = child.0.stdout.take().unwrap();
        let mut stdin = child.0.stdin.take().unwrap();
        let (lines_tx, lines_rx) = mpsc::sync_channel(16);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        });
        report(events, &request.key, "Слушаю слова · тайминги сохраняются");
        let mut last_update = Instant::now() - Duration::from_secs(1);
        while !self.cancelled(request) {
            if last_update.elapsed() >= Duration::from_millis(200) {
                let clock = self.clock(&request.key);
                if writeln!(stdin, "{}", serde_json::to_string(&clock)?).is_err() {
                    break;
                }
                last_update = Instant::now();
            }
            for line in lines_rx.try_iter() {
                if line.len() > 256 * 1024 {
                    continue;
                }
                let Ok(update) = serde_json::from_str::<Update>(&line) else {
                    continue;
                };
                let Some(base) = content.lines.get_mut(update.line) else {
                    continue;
                };
                if !valid_words(base, &update.words) {
                    continue;
                }
                let old = base.words.iter().filter(|w| w.end > w.start).count();
                let new = update.words.iter().filter(|w| w.end > w.start).count();
                if new == 0 || new < old || update.words == base.words {
                    continue;
                }
                base.words = update.words;
                if self.cancelled(request) {
                    break;
                }
                publish(events, &request.key, &content);
                let matched = content
                    .lines
                    .iter()
                    .flat_map(|l| &l.words)
                    .filter(|w| w.end > w.start)
                    .count();
                report(
                    events,
                    &request.key,
                    &format!("По голосу · {matched} слов сохранено"),
                );
                let mut file = tempfile::NamedTempFile::new_in(cache)?;
                serde_json::to_writer(
                    file.as_file_mut(),
                    &Cached {
                        lines: content.lines.clone(),
                    },
                )?;
                let _ = file.persist(&path);
            }
            if content.lines.iter().all(complete) {
                report(events, &request.key, "По словам · сохранённые тайминги");
                break;
            }
            if let Some(status) = child.0.try_wait()? {
                if !status.success() {
                    let detail = fs::read_to_string(&log)
                        .unwrap_or_default()
                        .lines()
                        .last()
                        .unwrap_or("не удалось получить звук воспроизведения")
                        .chars()
                        .take(180)
                        .collect::<String>();
                    anyhow::bail!("{detail}");
                }
                break;
            }
            thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    }
}
fn complete(line: &LyricLine) -> bool {
    !line.text.chars().any(char::is_alphanumeric)
        || (!line.words.is_empty()
            && line
                .words
                .iter()
                .filter(|w| w.text.chars().any(char::is_alphanumeric))
                .all(|w| w.end > w.start))
}
fn valid_words(line: &LyricLine, words: &[crate::lyrics::LyricWord]) -> bool {
    !words.is_empty()
        && words.len() <= 500
        && words.iter().map(|w| w.text.as_str()).collect::<String>() == line.text
        && words.iter().all(|w| {
            w.start.is_finite()
                && w.end.is_finite()
                && w.start >= line.start
                && w.end >= w.start
                && w.end <= line.end
        })
        && words
            .iter()
            .filter(|w| w.end > w.start)
            .map(|w| w.start)
            .collect::<Vec<_>>()
            .windows(2)
            .all(|w| w[0] <= w[1])
}
fn publish(events: &Sender<AssetEvent>, key: &str, content: &LyricsContent) {
    let mut content = content.clone();
    content.quality = "Audio word timing · Vosk (partial until heard)".into();
    let _ = events.send(AssetEvent::Lyrics {
        key: key.into(),
        content,
    });
}
fn report(events: &Sender<AssetEvent>, key: &str, message: &str) {
    let _ = events.send(AssetEvent::AlignmentStatus {
        key: key.into(),
        message: message.into(),
    });
}
struct Process(Child);
impl Drop for Process {
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
