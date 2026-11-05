//! CAVA's raw FFT output, running only while an instrumental is visible.
use std::{
    fs,
    io::Read,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const BARS: usize = 32;
type Frame = Arc<Mutex<([u8; BARS], Instant)>>;
#[derive(Default)]
pub struct Cava {
    session: Option<Session>,
    levels: [f32; BARS],
    attempted: Option<Instant>,
    failed: bool,
}
struct Session {
    child: Child,
    reader: Option<JoinHandle<()>>,
    frame: Frame,
    _directory: tempfile::TempDir,
}
impl Session {
    fn start() -> std::io::Result<Self> {
        let directory = tempfile::tempdir()?;
        let config = directory.path().join("config");
        fs::write(&config, format!("[general]\nframerate = 30\nbars = {BARS}\nautosens = 1\nsleep_timer = 2\n[input]\nmethod = pulse\nsource = auto\n[output]\nmethod = raw\nraw_target = /dev/stdout\ndata_format = binary\nbit_format = 8bit\nchannels = mono\nmono_option = average\n"))?;
        let mut child = Command::new("cava")
            .arg("-p")
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child.stdout.take().unwrap();
        let frame = Arc::new(Mutex::new(([0; BARS], Instant::now())));
        let target = frame.clone();
        let reader = thread::spawn(move || {
            let mut bars = [0; BARS];
            while stdout.read_exact(&mut bars).is_ok() {
                *target.lock().unwrap_or_else(|e| e.into_inner()) = (bars, Instant::now());
            }
        });
        Ok(Self {
            child,
            reader: Some(reader),
            frame,
            _directory: directory,
        })
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl Cava {
    pub fn update(&mut self, active: bool, dt: f32) {
        if !active {
            self.session.take();
            self.levels.fill(0.0);
            self.attempted = None;
            return;
        }
        if self
            .session
            .as_mut()
            .is_some_and(|s| !matches!(s.child.try_wait(), Ok(None)))
        {
            self.session.take();
            self.failed = true;
        }
        if self.session.is_none()
            && self
                .attempted
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            self.attempted = Some(Instant::now());
            match Session::start() {
                Ok(session) => {
                    self.session = Some(session);
                    self.failed = false;
                }
                Err(_) => self.failed = true,
            }
        }
        let target = self
            .session
            .as_ref()
            .map(|session| {
                let frame = session.frame.lock().unwrap_or_else(|e| e.into_inner());
                if frame.1.elapsed() < Duration::from_millis(500) {
                    frame.0
                } else {
                    [0; BARS]
                }
            })
            .unwrap_or([0; BARS]);
        let blend = 1.0 - (-dt.max(0.0) * 18.0).exp();
        for (level, value) in self.levels.iter_mut().zip(target) {
            *level += (f32::from(value) / 255.0 - *level) * blend;
        }
    }
    pub fn levels(&self) -> &[f32; BARS] {
        &self.levels
    }
    pub fn failed(&self) -> bool {
        self.failed
    }
    pub fn stop(&mut self) {
        self.update(false, 0.0);
    }
}
