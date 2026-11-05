//! Owned, audio-only mpv process controlled exclusively through JSON IPC.
//!
//! Construction and idle snapshots do not start a process. Like the rest of
//! `PlaybackBackend`, commands belong on the caller's I/O worker. A separate
//! reader continuously handles property events while commands await replies.

use super::{
    Capabilities, PlaybackBackend, PlaybackCommand, PlaybackSnapshot, PlaybackStatus, Track,
};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const SOURCE_ID: &str = "native";
const YTDL_RESULT: &str = "user-data/mpv/ytdl/json-subprocess-result";
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const PROPERTIES: &[&str] = &[
    "path",
    "media-title",
    "metadata",
    "duration",
    "time-pos",
    "pause",
    "idle-active",
    "core-idle",
    "eof-reached",
    "seekable",
    "volume",
    "speed",
    "playlist-pos",
    "playlist-count",
    "track-list",
    YTDL_RESULT,
];
const FILE_PROPERTIES: &[&str] = &[
    "path",
    "media-title",
    "metadata",
    "duration",
    "time-pos",
    "core-idle",
    "eof-reached",
    "seekable",
    "track-list",
    YTDL_RESULT,
];

/// Launch configuration. Production defaults use the normal audio device.
/// Tests can inject an executable and `extra_args: vec!["--ao=null".into()]`.
/// Extra arguments precede the fixed IPC/audio-only arguments, and may also
/// supply an interpreter's script path when injecting a mock executable.
#[derive(Clone, Debug)]
pub struct NativeOptions {
    pub mpv_path: PathBuf,
    pub extra_args: Vec<OsString>,
    pub startup_timeout: Duration,
    pub command_timeout: Duration,
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            mpv_path: "mpv".into(),
            extra_args: Vec::new(),
            startup_timeout: Duration::from_secs(3),
            command_timeout: Duration::from_secs(2),
        }
    }
}

pub struct NativeBackend {
    options: NativeOptions,
    session: Option<Session>,
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeBackend {
    pub fn new() -> Self {
        Self::with_options(NativeOptions::default())
    }

    pub fn with_options(options: NativeOptions) -> Self {
        Self {
            options,
            session: None,
        }
    }

    fn session(&mut self) -> Result<&mut Session> {
        // An explicit Open (or volume adjustment) may restart an exited child.
        if self
            .session
            .as_ref()
            .is_some_and(|s| lock(&s.shared).disconnected.is_some())
        {
            self.session.take();
        }
        if self.session.is_none() {
            self.session = Some(Session::start(&self.options)?);
        }
        Ok(self.session.as_mut().expect("session was initialized"))
    }
}

impl PlaybackBackend for NativeBackend {
    fn refresh(&mut self) -> Result<Vec<PlaybackSnapshot>> {
        let snapshot = self
            .session
            .as_ref()
            .map(|s| lock(&s.shared).state.snapshot.clone())
            .unwrap_or_else(idle_snapshot);
        Ok(vec![snapshot])
    }

    fn changed(&mut self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| std::mem::take(&mut lock(&session.shared).changed))
    }

    fn command(&mut self, source_id: &str, command: PlaybackCommand) -> Result<()> {
        if source_id != SOURCE_ID {
            bail!("unknown native source: {source_id}");
        }
        match command {
            PlaybackCommand::Open(input) => {
                if input.trim().is_empty() || input.contains('\0') {
                    bail!("a nonempty URL or local path is required");
                }
                let session = self.session()?;
                // loadfile is accepted immediately; mpv's ytdl hook performs
                // extraction on mpv's side, without a second extractor process.
                session.request(json!(["loadfile", input, "replace"]))?;
                session.request(json!(["set_property", "pause", false]))?;
            }
            PlaybackCommand::Volume(volume) => {
                if !volume.is_finite() {
                    bail!("volume must be finite");
                }
                self.session()?.request(json!([
                    "set_property",
                    "volume",
                    volume.clamp(0.0, 1.0) * 100.0
                ]))?;
            }
            other => {
                let session = self.session.as_mut().context("no native track is loaded")?;
                let snapshot = lock(&session.shared).state.snapshot.clone();
                match other {
                    PlaybackCommand::Play | PlaybackCommand::Pause | PlaybackCommand::PlayPause => {
                        if !snapshot.capabilities.pause {
                            bail!("native playback cannot currently pause or resume");
                        }
                        let command = match other {
                            PlaybackCommand::Play => json!(["set_property", "pause", false]),
                            PlaybackCommand::Pause => json!(["set_property", "pause", true]),
                            _ => json!(["cycle", "pause"]),
                        };
                        session.request(command)?;
                    }
                    PlaybackCommand::Seek(offset) | PlaybackCommand::SetPosition(offset) => {
                        if !offset.is_finite() {
                            bail!("seek position must be finite");
                        }
                        if !snapshot.capabilities.seek {
                            bail!("native track is not seekable");
                        }
                        let mut target = if matches!(other, PlaybackCommand::Seek(_)) {
                            snapshot.position_now() + offset
                        } else {
                            offset
                        };
                        target = target.max(0.0);
                        if snapshot.track.duration > 0.0 {
                            target = target.min(snapshot.track.duration);
                        }
                        session.request(json!(["seek", target, "absolute+exact"]))?;
                    }
                    PlaybackCommand::Next | PlaybackCommand::Previous => {
                        // Query at execution time: cached observations can lag
                        // an automatic playlist transition. Weak mode also
                        // guards the boundary inside mpv, without stopping it.
                        let pos = session
                            .request(json!(["get_property", "playlist-pos"]))?
                            .as_i64()
                            .unwrap_or(-1);
                        let count = session
                            .request(json!(["get_property", "playlist-count"]))?
                            .as_i64()
                            .unwrap_or(0);
                        let next = matches!(other, PlaybackCommand::Next);
                        if pos >= 0 && pos < count && (if next { pos < count - 1 } else { pos > 0 })
                        {
                            session.request(json!([
                                if next {
                                    "playlist-next"
                                } else {
                                    "playlist-prev"
                                },
                                "weak"
                            ]))?;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
        Ok(())
    }
}

fn idle_snapshot() -> PlaybackSnapshot {
    PlaybackSnapshot {
        source_id: SOURCE_ID.into(),
        source_name: "mpv".into(),
        capabilities: Capabilities {
            volume: true,
            ..Capabilities::default()
        },
        ..PlaybackSnapshot::default()
    }
}

// Poisoning must not make Drop panic and strand the owned process.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct PlayerState {
    properties: HashMap<String, Value>,
    snapshot: PlaybackSnapshot,
    loading: bool,
    ended: bool,
    // Parsed only when the ytdl property changes, never during refresh().
    ytdl: Option<Value>,
}

impl PlayerState {
    fn new() -> Self {
        Self {
            properties: HashMap::new(),
            snapshot: idle_snapshot(),
            loading: false,
            ended: false,
            ytdl: None,
        }
    }

    fn advance_clock(&mut self) {
        self.snapshot.position = self.snapshot.position_now();
        self.snapshot.updated_at = Instant::now();
    }

    fn clear_file(&mut self) {
        for property in FILE_PROPERTIES {
            self.properties.remove(*property);
        }
        self.ytdl = None;
        self.snapshot.track = Track::default();
        self.snapshot.position = 0.0;
        self.snapshot.updated_at = Instant::now();
    }

    fn property(&mut self, name: &str, data: Value) {
        self.advance_clock();
        if name == "path"
            && data.as_str().is_some()
            && self
                .properties
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|old| Some(old) != data.as_str())
        {
            // Also handle servers that report a changed path without a
            // start-file event. Never carry artwork or tags to the new file.
            self.clear_file();
        }
        if name == YTDL_RESULT {
            self.ytdl = data
                .get("stdout")
                .and_then(Value::as_str)
                .and_then(|stdout| serde_json::from_str(stdout).ok());
        }
        if name == "time-pos" {
            self.snapshot.position = finite_number(&data).unwrap_or(0.0).max(0.0);
        }
        self.properties.insert(name.into(), data);
        self.rebuild();
    }

    fn event(&mut self, message: &Value) -> bool {
        match message.get("event").and_then(Value::as_str) {
            Some("property-change") => {
                if let Some(name) = message.get("name").and_then(Value::as_str) {
                    if PROPERTIES.contains(&name) {
                        self.property(name, message.get("data").cloned().unwrap_or(Value::Null));
                        return true;
                    }
                }
                false
            }
            Some("start-file") => {
                self.clear_file();
                self.loading = true;
                self.ended = false;
                self.properties.insert("idle-active".into(), json!(false));
                self.rebuild();
                true
            }
            Some("file-loaded") => {
                self.advance_clock();
                self.loading = false;
                self.ended = false;
                self.properties.insert("idle-active".into(), json!(false));
                self.rebuild();
                true
            }
            Some("end-file") => {
                self.clear_file();
                self.loading = false;
                self.ended = true;
                self.rebuild();
                true
            }
            Some("playback-restart") | Some("seek") => {
                self.advance_clock();
                true
            }
            _ => false,
        }
    }

    fn rebuild(&mut self) {
        let path = self
            .properties
            .get("path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let active = path.is_some() && !self.ended && !self.flag("idle-active", true);
        let loaded = active && !self.loading;
        let paused = self.flag("pause", false) || self.flag("core-idle", false);
        self.snapshot.status = if !loaded {
            PlaybackStatus::Stopped
        } else if paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
        self.snapshot.rate = self.number("speed").filter(|n| *n > 0.0).unwrap_or(1.0);
        self.snapshot.volume = self.number("volume").unwrap_or(100.0).max(0.0) / 100.0;
        let pos = self
            .properties
            .get("playlist-pos")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let count = self
            .properties
            .get("playlist-count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        self.snapshot.capabilities = Capabilities {
            play: loaded,
            pause: loaded,
            seek: loaded && self.flag("seekable", false),
            next: pos >= 0 && pos < count.saturating_sub(1),
            previous: pos > 0 && pos < count,
            volume: true,
        };
        if !active {
            self.snapshot.track = Track::default();
            self.snapshot.position = 0.0;
            return;
        }
        let path = path.unwrap();
        let metadata = self.properties.get("metadata").unwrap_or(&Value::Null);
        let ytdl = self.ytdl.as_ref().unwrap_or(&Value::Null);
        let title = tag(metadata, &["title"])
            .or_else(|| tag(ytdl, &["track", "title"]))
            .or_else(|| {
                self.properties
                    .get("media-title")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path)
                    .to_owned()
            });
        let artist = tag(
            metadata,
            &["artist", "album_artist", "album-artist", "uploader"],
        )
        .or_else(|| tag(ytdl, &["artist", "album_artist", "uploader", "channel"]))
        .unwrap_or_default();
        let album = tag(metadata, &["album"])
            .or_else(|| tag(ytdl, &["album"]))
            .unwrap_or_default();
        let artwork = tag(
            metadata,
            &[
                "artwork_url",
                "artwork",
                "cover_url",
                "coverart",
                "thumbnail",
            ],
        )
        .or_else(|| tag(ytdl, &["thumbnail"]))
        .or_else(|| {
            ytdl.get("thumbnails")
                .and_then(Value::as_array)
                .and_then(|items| items.iter().rev().find_map(|item| tag(item, &["url"])))
        })
        .or_else(|| {
            self.properties
                .get("track-list")
                .and_then(Value::as_array)
                .and_then(|tracks| {
                    tracks.iter().find_map(|track| {
                        if track.get("image").and_then(Value::as_bool).unwrap_or(false)
                            || track
                                .get("albumart")
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                        {
                            tag(track, &["external-filename"])
                        } else {
                            None
                        }
                    })
                })
        });
        self.snapshot.track = Track {
            id: format!("native:{path}"),
            title,
            artist,
            album,
            artwork_url: artwork,
            url: Some(path.into()),
            duration: self
                .number("duration")
                .or_else(|| ytdl.get("duration").and_then(finite_number))
                .unwrap_or(0.0)
                .max(0.0),
        };
    }

    fn flag(&self, key: &str, fallback: bool) -> bool {
        self.properties
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(fallback)
    }

    fn number(&self, key: &str) -> Option<f64> {
        self.properties.get(key).and_then(finite_number)
    }
}

fn finite_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite())
}

fn tag(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    keys.iter().find_map(|wanted| {
        object.iter().find_map(|(key, value)| {
            if key.eq_ignore_ascii_case(wanted) {
                value
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
            } else {
                None
            }
        })
    })
}

type Reply = std::result::Result<Value, String>;
struct Shared {
    state: PlayerState,
    pending: HashMap<u64, mpsc::SyncSender<Reply>>,
    changed: bool,
    disconnected: Option<String>,
}

impl Shared {
    fn disconnect(&mut self, reason: String) {
        if self.disconnected.is_some() {
            return;
        }
        self.disconnected = Some(reason.clone());
        self.state.clear_file();
        self.state.ended = true;
        self.state.rebuild();
        self.state.snapshot.capabilities = Capabilities::default();
        self.changed = true;
        for (_, sender) in self.pending.drain() {
            let _ = sender.send(Err(reason.clone()));
        }
    }
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Give quit a short grace period, then terminate and reap even when
        // mpv is unresponsive or startup/observation failed halfway through.
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Session {
    // The directory outlives both the socket and child.
    _directory: TempDir,
    child: Option<OwnedChild>,
    writer: UnixStream,
    reader: Option<JoinHandle<()>>,
    shared: Arc<Mutex<Shared>>,
    next_id: u64,
    timeout: Duration,
}

impl Session {
    fn start(options: &NativeOptions) -> Result<Self> {
        // A short, private path avoids Unix's sockaddr_un length limit even
        // when TMPDIR or the working directory has a long name.
        let directory = tempfile::Builder::new()
            .prefix("lyrictty-mpv-")
            .tempdir_in("/tmp")
            .context("create private mpv IPC directory")?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let socket = directory.path().join("ipc.sock");
        let child = Command::new(&options.mpv_path)
            .args(&options.extra_args)
            .args([
                "--no-config",
                "--idle=yes",
                "--no-video",
                "--no-terminal",
                "--input-terminal=no",
                "--input-default-bindings=no",
                "--osc=no",
                "--audio-display=no",
                "--ytdl=yes",
                "--script-opts=ytdl_hook-ytdl_path=yt-dlp",
                "--keep-open=no",
            ])
            .arg(format!("--input-ipc-server={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("launch mpv ({})", options.mpv_path.display()))?;
        let mut child = OwnedChild(child);
        let deadline = Instant::now() + options.startup_timeout;
        let writer = loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => break stream,
                Err(error) => {
                    if let Some(status) = child.0.try_wait()? {
                        bail!("mpv exited before IPC was ready: {status}");
                    }
                    if Instant::now() >= deadline {
                        return Err(error).context("timed out waiting for mpv IPC socket");
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
        };
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        writer.set_write_timeout(Some(options.command_timeout.max(Duration::from_millis(1))))?;
        let input = writer.try_clone()?;
        let shared = Arc::new(Mutex::new(Shared {
            state: PlayerState::new(),
            pending: HashMap::new(),
            changed: false,
            disconnected: None,
        }));
        let reader_shared = Arc::clone(&shared);
        let reader = thread::Builder::new()
            .name("mpv-json-ipc".into())
            .spawn(move || read_messages(input, reader_shared))
            .context("start mpv IPC reader")?;
        let mut session = Self {
            _directory: directory,
            child: Some(child),
            writer,
            reader: Some(reader),
            shared,
            next_id: 1,
            timeout: options.command_timeout,
        };
        for (id, property) in PROPERTIES.iter().enumerate() {
            if let Err(error) = session.request(json!(["observe_property", id + 1, property])) {
                // Older mpv versions may lack ytdl's optional user-data node.
                if *property != YTDL_RESULT {
                    return Err(error).with_context(|| format!("observe mpv {property}"));
                }
            }
        }
        Ok(session)
    }

    fn request(&mut self, command: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .filter(|id| *id <= i64::MAX as u64)
            .context("mpv request ID exhausted")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut shared = lock(&self.shared);
            if let Some(error) = &shared.disconnected {
                bail!("mpv IPC disconnected: {error}");
            }
            shared.pending.insert(id, sender);
        }
        let mut bytes = serde_json::to_vec(&json!({"command": command, "request_id": id}))?;
        bytes.push(b'\n');
        if let Err(error) = self.writer.write_all(&bytes) {
            lock(&self.shared).disconnect(format!("mpv IPC write failed: {error}"));
            return Err(error).context("send mpv IPC command");
        }
        let name = command.get(0).and_then(Value::as_str).unwrap_or("unknown");
        match receiver.recv_timeout(self.timeout) {
            Ok(Ok(data)) => Ok(data),
            Ok(Err(error)) => Err(anyhow!("mpv command {name} failed: {error}")),
            Err(error) => {
                // Late replies are ignored, never consumed by the next call.
                lock(&self.shared).pending.remove(&id);
                Err(anyhow!(
                    "mpv command {name} (request {id}) did not reply: {error}"
                ))
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self
            .writer
            .set_write_timeout(Some(Duration::from_millis(50)));
        let _ = self
            .writer
            .write_all(b"{\"command\":[\"quit\"],\"request_id\":0}\n");
        let _ = self.writer.shutdown(Shutdown::Both);
        lock(&self.shared).disconnect("native backend shut down".into());
        // shutdown wakes the blocking read on the cloned socket. No worker
        // waits on a channel send or a child's stdout/stderr during teardown.
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.child.take();
    }
}

fn read_messages(mut input: UnixStream, shared: Arc<Mutex<Shared>>) {
    let result = (|| -> Result<()> {
        let mut buffer = [0u8; 8192];
        let mut line = Vec::new();
        loop {
            let count = match input.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result.context("read mpv IPC")?,
            };
            if count == 0 {
                bail!("mpv closed the IPC socket");
            }
            for byte in &buffer[..count] {
                if *byte != b'\n' {
                    if line.len() >= MAX_MESSAGE_BYTES {
                        bail!("mpv IPC message exceeds size limit");
                    }
                    line.push(*byte);
                    continue;
                }
                if !line.iter().all(u8::is_ascii_whitespace) {
                    // mpv documents that some file tags contain broken UTF-8.
                    let message: Value = serde_json::from_str(&String::from_utf8_lossy(&line))
                        .context("invalid mpv IPC JSON")?;
                    let mut shared = lock(&shared);
                    if message.get("event").is_some() {
                        let changed = shared.state.event(&message);
                        shared.changed |= changed;
                        if message.get("event").and_then(Value::as_str) == Some("shutdown") {
                            shared.disconnect("mpv shut down".into());
                        }
                    } else if let Some(id) = message.get("request_id").and_then(Value::as_u64) {
                        if let Some(sender) = shared.pending.remove(&id) {
                            let response = match message.get("error").and_then(Value::as_str) {
                                Some("success") => {
                                    Ok(message.get("data").cloned().unwrap_or(Value::Null))
                                }
                                Some(error) => Err(error.into()),
                                None => Err("IPC response has no error field".into()),
                            };
                            let _ = sender.send(response);
                        }
                    }
                }
                line.clear();
            }
        }
    })();
    if let Err(error) = result {
        lock(&shared).disconnect(format!("{error:#}"));
    }
}
