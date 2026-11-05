use super::{PlaybackBackend, PlaybackCommand, PlaybackSnapshot, Track};
use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub enum Request {
    Upcoming {
        source_id: String,
        track_id: String,
        limit: usize,
    },
    Command {
        source_id: String,
        command: PlaybackCommand,
    },
    Shutdown,
}
pub enum Event {
    Upcoming {
        source_id: String,
        track_id: String,
        tracks: Vec<Track>,
    },
    Sources(Vec<PlaybackSnapshot>),
    Error(String),
}
pub struct PlaybackService {
    pub requests: Sender<Request>,
    pub events: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
}
impl PlaybackService {
    /// The factory runs on the worker so D-Bus setup cannot stall terminal input.
    pub fn start(
        factory: impl FnOnce() -> (Vec<Box<dyn PlaybackBackend>>, Vec<String>) + Send + 'static,
    ) -> Self {
        let (requests, rx) = mpsc::channel();
        let (tx, events) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut backends, errors) = factory();
            for error in errors {
                let _ = tx.send(Event::Error(error));
            }
            let mut snapshots: Vec<Vec<PlaybackSnapshot>> = vec![Vec::new(); backends.len()];
            let mut first = true;
            let mut resync = Instant::now();
            let mut force = false;
            loop {
                let mut changed = false;
                // Position is locally extrapolated. Five-second local resync handles
                // imperfect MPRIS implementations without polling metadata/network.
                let timed = resync.elapsed() >= Duration::from_secs(5);
                for (i, backend) in backends.iter_mut().enumerate() {
                    let event = backend.changed();
                    if first || force || event || timed {
                        match backend.refresh() {
                            Ok(v) => snapshots[i] = v,
                            Err(_) => {
                                snapshots[i].clear();
                                let _ = tx.send(Event::Error("Source disconnected".into()));
                            }
                        }
                        changed = true;
                    }
                }
                if changed
                    && tx
                        .send(Event::Sources(
                            snapshots.iter().flatten().cloned().collect(),
                        ))
                        .is_err()
                {
                    break;
                }
                if timed {
                    resync = Instant::now();
                }
                first = false;
                force = false;
                match rx.recv_timeout(Duration::from_millis(80)) {
                    Ok(Request::Upcoming {
                        source_id,
                        track_id,
                        limit,
                    }) => {
                        let tracks = snapshots
                            .iter()
                            .position(|sources| sources.iter().any(|s| s.source_id == source_id))
                            .and_then(|index| {
                                backends[index].upcoming(&source_id, limit.min(3)).ok()
                            })
                            .unwrap_or_default();
                        let _ = tx.send(Event::Upcoming {
                            source_id,
                            track_id,
                            tracks,
                        });
                    }
                    Ok(Request::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(Request::Command { source_id, command }) => {
                        let index = snapshots
                            .iter()
                            .position(|sources| sources.iter().any(|s| s.source_id == source_id));
                        if let Some(index) = index {
                            if let Err(e) = backends[index].command(&source_id, command) {
                                // Backend errors must not echo signed URLs or user paths.
                                let _ = e;
                                let _=tx.send(Event::Error("Playback command failed or unavailable; check source capabilities".into()));
                            }
                            force = true;
                        } else {
                            let _ = tx.send(Event::Error("Source disconnected".into()));
                        }
                    }
                }
            }
        });
        Self {
            requests,
            events,
            worker: Some(worker),
        }
    }
    pub fn command(&self, source_id: &str, command: PlaybackCommand) {
        let _ = self.requests.send(Request::Command {
            source_id: source_id.into(),
            command,
        });
    }
    pub fn upcoming(&self, source_id: &str, track_id: &str, limit: usize) {
        let _ = self.requests.send(Request::Upcoming {
            source_id: source_id.into(),
            track_id: track_id.into(),
            limit: limit.min(3),
        });
    }
}
impl Drop for PlaybackService {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
