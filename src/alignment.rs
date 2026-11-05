//! Prepare acoustic lyric cues from an independent audio file before playback.
use crate::{assets::AssetEvent, content::LyricsContent, lyrics::LyricLine, playback::Track};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader},
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
    generation: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    worker: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    prefetch: Arc<Mutex<VecDeque<Request>>>,
    prefetch_epoch: Arc<AtomicU64>,
    foreground_waiting: Arc<AtomicBool>,
    threads: usize,
}
#[derive(Clone)]
struct Request {
    key: String,
    track: Track,
    content: LyricsContent,
    generation: u64,
    prefetch_epoch: Option<u64>,
}
#[derive(Serialize, Deserialize)]
struct Cached {
    #[serde(default)]
    prepared: bool,
    lines: Vec<LyricLine>,
}
#[derive(Deserialize)]
struct Update {
    line: Option<usize>,
    #[serde(default)]
    words: Vec<crate::lyrics::LyricWord>,
    status: Option<String>,
    #[serde(default)]
    prepared: bool,
}

impl AlignmentHandle {
    pub fn start(cache: PathBuf, events: Sender<AssetEvent>) -> Self {
        Self::start_with_threads(cache, events, 0)
    }
    pub fn start_with_threads(
        cache: PathBuf,
        events: Sender<AssetEvent>,
        requested: usize,
    ) -> Self {
        let (requests, rx) = mpsc::channel::<Request>();
        let available = thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let threads = if requested == 0 {
            available
        } else {
            requested.min(available).max(1)
        };
        let handle = Self {
            requests,
            generation: Arc::new(AtomicU64::new(0)),
            stopped: Arc::new(AtomicBool::new(false)),
            worker: Arc::new(Mutex::new(None)),
            prefetch: Arc::new(Mutex::new(VecDeque::new())),
            prefetch_epoch: Arc::new(AtomicU64::new(0)),
            foreground_waiting: Arc::new(AtomicBool::new(false)),
            threads,
        };
        let worker = handle.clone();
        let task = thread::spawn(move || {
            while !worker.stopped.load(Ordering::Relaxed) {
                let foreground = rx.try_recv().ok();
                let mut request = if let Some(request) = foreground {
                    let mut latest = request;
                    while let Ok(next) = rx.try_recv() {
                        latest = next;
                    }
                    if worker.generation.load(Ordering::Relaxed) != latest.generation {
                        continue;
                    }
                    worker.foreground_waiting.store(false, Ordering::Relaxed);
                    latest
                } else if !worker.foreground_waiting.load(Ordering::Relaxed) {
                    let next = worker
                        .prefetch
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pop_front();
                    let Some(mut next) = next else {
                        thread::sleep(Duration::from_millis(80));
                        continue;
                    };
                    next.generation = worker.generation.load(Ordering::Relaxed);
                    next
                } else {
                    thread::sleep(Duration::from_millis(40));
                    continue;
                };
                let result = if worker.cancelled(&request) {
                    Ok(())
                } else {
                    worker.align(&request, &cache, &events)
                };
                if let Some(epoch) = request.prefetch_epoch {
                    if worker.cancelled(&request) {
                        if !worker.stopped.load(Ordering::Relaxed)
                            && worker.prefetch_epoch.load(Ordering::Relaxed) == epoch
                        {
                            request.generation = worker.generation.load(Ordering::Relaxed);
                            worker
                                .prefetch
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push_front(request);
                        }
                    } else {
                        let message = if result.is_ok() {
                            format!("Подготовлено заранее: {}", request.track.title)
                        } else {
                            format!("Не удалось подготовить заранее: {}", request.track.title)
                        };
                        let _ = events.send(AssetEvent::PrefetchStatus { message });
                    }
                } else if !worker.cancelled(&request) {
                    if let Err(error) = &result {
                        report(&events, &request.key, &format!("Подготовка слов: {error}"));
                    }
                    let _ = events.send(AssetEvent::AlignmentReady {
                        key: request.key.clone(),
                        prepared: result.is_ok(),
                    });
                }
            }
        });
        *handle.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        handle
    }
    pub fn invalidate(&self) -> u64 {
        self.foreground_waiting.store(true, Ordering::Relaxed);
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
            prefetch_epoch: None,
        });
    }
    pub fn begin_prefetch(&self) -> u64 {
        let epoch = self.prefetch_epoch.fetch_add(1, Ordering::Relaxed) + 1;
        self.prefetch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        epoch
    }
    pub fn prefetch(&self, track: Track, content: LyricsContent, epoch: u64) {
        if self.prefetch_epoch.load(Ordering::Relaxed) != epoch
            || self.stopped.load(Ordering::Relaxed)
        {
            return;
        }
        let mut queue = self.prefetch.lock().unwrap_or_else(|e| e.into_inner());
        if queue.len() < 3 {
            queue.push_back(Request {
                key: format!("prefetch:{epoch}:{}", track.id),
                track,
                content,
                generation: self.generation.load(Ordering::Relaxed),
                prefetch_epoch: Some(epoch),
            });
        }
    }
    fn cancelled(&self, request: &Request) -> bool {
        self.stopped.load(Ordering::Relaxed)
            || self.generation.load(Ordering::Relaxed) != request.generation
            || request.prefetch_epoch.is_some_and(|epoch| {
                self.prefetch_epoch.load(Ordering::Relaxed) != epoch
                    || self.foreground_waiting.load(Ordering::Relaxed)
            })
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
        let identity = crate::metadata::lyrics_query(&request.track);
        let fingerprint = serde_json::to_vec(&(
            &identity.artist,
            &identity.title,
            &identity.album,
            request.track.duration.round() as i64,
            &request.content.lines,
        ))?;
        let path = cache.join(format!("{:x}.json", Sha256::digest(fingerprint)));
        let legacy = serde_json::to_vec(&(
            &request.track.artist,
            &request.track.title,
            &request.track.album,
            request.track.duration.round() as i64,
            &request.track.url,
            &request.content.lines,
        ))?;
        let legacy_path = cache.join(format!("{:x}.json", Sha256::digest(legacy)));
        let mut content = request.content.clone();
        if let Ok(bytes) = fs::read(&path).or_else(|_| fs::read(&legacy_path)) {
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
                        if cached.prepared && content.lines.iter().all(complete) {
                            if !path.exists() {
                                save(cache, &path, &content, true)?;
                            }
                            publish(events, &request.key, &content);
                            report(events, &request.key, "По словам · полная разметка из кэша");
                            return Ok(());
                        }
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
        let python = root.join("venv/bin/python");
        let models = root.join("models/stable-ts");
        let model_name = "base";
        anyhow::ensure!(
            python.exists() && models.join("base.pt").exists(),
            "нужен stable-ts; запусти scripts/setup-alignment.sh base"
        );
        let temp = tempfile::tempdir()?;
        let helper = temp.path().join("align.py");
        let input = temp.path().join("request.json");
        fs::write(&helper, include_str!("../scripts/align_lyrics.py"))?;
        fs::write(
            temp.path().join("prepare_audio.py"),
            include_str!("../scripts/prepare_audio.py"),
        )?;
        fs::write(
            &input,
            serde_json::to_vec(
                &serde_json::json!({"lines":content.lines, "track":request.track, "work_dir":temp.path(), "position":0.0, "playing":false, "rate":1.0}),
            )?,
        )?;
        let log = temp.path().join("error.log");
        let mut command = Command::new(python);
        command
            .args(["-u"])
            .arg(helper)
            .arg("--offline")
            .arg("--request")
            .arg(input)
            .arg("--model")
            .arg(model_name)
            .arg("--models-dir")
            .arg(models)
            .env("LYRICTTY_ALIGNMENT_THREADS", self.threads.to_string())
            .env("OMP_NUM_THREADS", self.threads.to_string())
            .env("OPENBLAS_NUM_THREADS", self.threads.to_string())
            .env("MKL_NUM_THREADS", self.threads.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(fs::File::create(&log)?));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = Process(command.spawn()?);
        let stdout = child.0.stdout.take().unwrap();
        let (lines_tx, lines_rx) = mpsc::sync_channel(16);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if lines_tx.send(Some(line)).is_err() {
                    return;
                }
            }
            let _ = lines_tx.send(None);
        });
        report(
            events,
            &request.key,
            &format!("Подготавливаю аудио · {} потоков CPU", self.threads),
        );
        let started = Instant::now();
        let mut output_closed = false;
        let mut prepared = false;
        while !self.cancelled(request) {
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(300),
                "превышено время подготовки аудио"
            );
            for line in lines_rx.try_iter() {
                let Some(line) = line else {
                    output_closed = true;
                    continue;
                };
                if line.len() > 256 * 1024 {
                    continue;
                }
                let Ok(update) = serde_json::from_str::<Update>(&line) else {
                    continue;
                };
                if let Some(status) = update.status {
                    let message = match status.as_str() {
                        "preparing_audio" => "Подготавливаю аудио…",
                        "searching_audio" => "Ищу запись по названию и длительности…",
                        "downloading_audio" => "Загружаю аудио для разметки…",
                        "validating_audio" => "Проверяю совпадение записи…",
                        "decoding_audio" => "Подготавливаю звук для анализа…",
                        "loading_model" => "Загружаю модель…",
                        "aligning_lyrics" => "Сопоставляю слова с аудио…",
                        retry if retry.starts_with("retrying_line_") => {
                            "Уточняю тайминги сложной строки…"
                        }
                        _ => "Подготавливаю тайминги слов…",
                    };
                    report(events, &request.key, message);
                }
                if update.prepared {
                    prepared = true;
                }
                let Some(index) = update.line else {
                    continue;
                };
                let Some(base) = content.lines.get_mut(index) else {
                    continue;
                };
                if !valid_words(base, &update.words) || !all_tokens_timed(&update.words) {
                    continue;
                }
                let old = base.words.iter().filter(|w| w.is_timed()).count();
                let new = update.words.iter().filter(|w| w.is_timed()).count();
                if new == 0 || new < old || update.words == base.words {
                    continue;
                }
                base.words = update.words;
                if self.cancelled(request) {
                    break;
                }
                let matched = matched_words(&content);
                report(
                    events,
                    &request.key,
                    &format!("Подготавливаю разметку · {matched} слов"),
                );
                publish(events, &request.key, &content);
                save(cache, &path, &content, false)?;
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
                if output_closed {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(40));
        }
        if self.cancelled(request) {
            return Ok(());
        }
        anyhow::ensure!(
            prepared && content.lines.iter().all(complete),
            "полная разметка не получена: часть слов не имеет корректных таймингов"
        );
        save(cache, &path, &content, true)?;
        publish(events, &request.key, &content);
        report(
            events,
            &request.key,
            &format!(
                "Готово · {} слов · {:.1} с",
                matched_words(&content),
                started.elapsed().as_secs_f64()
            ),
        );
        Ok(())
    }
}
fn matched_words(content: &LyricsContent) -> usize {
    content
        .lines
        .iter()
        .flat_map(|l| &l.words)
        .filter(|w| w.is_timed())
        .count()
}
fn save(cache: &Path, path: &Path, content: &LyricsContent, prepared: bool) -> anyhow::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(cache)?;
    serde_json::to_writer(
        file.as_file_mut(),
        &Cached {
            lines: content.lines.clone(),
            prepared,
        },
    )?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn all_tokens_timed(words: &[crate::lyrics::LyricWord]) -> bool {
    words
        .iter()
        .filter(|w| w.text.chars().any(char::is_alphanumeric))
        .all(|w| w.is_timed())
}
fn complete(line: &LyricLine) -> bool {
    !line.text.chars().any(char::is_alphanumeric)
        || (valid_words(line, &line.words) && all_tokens_timed(&line.words))
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
                && (!w.point || w.end == w.start)
                && w.end <= line.end
        })
        && words
            .iter()
            .filter(|w| w.is_timed())
            .map(|w| w.start)
            .collect::<Vec<_>>()
            .windows(2)
            .all(|w| w[0] <= w[1])
}
fn publish(events: &Sender<AssetEvent>, key: &str, content: &LyricsContent) {
    let mut content = content.clone();
    content.quality = "Stable-ts alignment · original text preserved".into();
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
