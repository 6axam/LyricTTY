//! Run against a private bus, never the user's actual media players.
use lyrictty::playback::{
    mpris::MprisBackend, PlaybackBackend, PlaybackCommand, PlaybackSnapshot, PlaybackStatus,
};

use anyhow::Result;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use zbus::{
    blocking::{connection::Builder, Connection},
    fdo,
    zvariant::{ObjectPath, OwnedValue, Value},
};

const NAME: &str = "org.mpris.MediaPlayer2.LyricTTYTest";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const ROOT: &str = "org.mpris.MediaPlayer2";
type Properties = HashMap<String, OwnedValue>;

struct Bus(Child);

impl Bus {
    fn start() -> Result<(Self, String)> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut address)?;
        Ok((Self(child), address.trim().into()))
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Default)]
struct State {
    identity_reads: usize,
    metadata_reads: usize,
    position_reads: usize,
    position: i64,
    volume: f64,
    blocked: bool,
    no_track: bool,
    set_position_error: u8,
    calls: Vec<String>,
}

struct MockRoot(Arc<Mutex<State>>);

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl MockRoot {
    #[zbus(property)]
    fn identity(&self) -> String {
        self.0.lock().unwrap().identity_reads += 1;
        "Mock Media Player".into()
    }
}

struct MockPlayer(Arc<Mutex<State>>);

impl MockPlayer {
    fn record(&self, name: &str) {
        self.0.lock().unwrap().calls.push(name.into());
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl MockPlayer {
    fn play(&self) {
        self.record("Play");
    }
    fn pause(&self) {
        self.record("Pause");
    }
    fn play_pause(&self) {
        self.record("PlayPause");
    }
    fn next(&self) {
        self.record("Next");
    }
    fn previous(&self) {
        self.record("Previous");
    }
    fn open_uri(&self, uri: String) {
        self.record(&format!("OpenUri:{uri}"));
    }

    fn seek(&self, offset: i64) {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("Seek:{offset}"));
        state.position += offset;
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) -> fdo::Result<()> {
        let mut state = self.0.lock().unwrap();
        state
            .calls
            .push(format!("SetPosition:{track_id}:{position}"));
        match state.set_position_error {
            1 => return Err(fdo::Error::UnknownMethod("unsupported".into())),
            2 => return Err(fdo::Error::Failed("real failure".into())),
            _ => {}
        }
        state.position = position;
        Ok(())
    }

    #[zbus(property)]
    fn playback_status(&self) -> &str {
        "Paused"
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.25
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.0.lock().unwrap().volume
    }
    #[zbus(property)]
    fn set_volume(&mut self, volume: f64) {
        self.0.lock().unwrap().volume = volume;
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        let mut state = self.0.lock().unwrap();
        state.position_reads += 1;
        state.position
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        !self.0.lock().unwrap().blocked
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn metadata(&self) -> Properties {
        let mut state = self.0.lock().unwrap();
        state.metadata_reads += 1;
        let mut metadata = Properties::from([
            ("xesam:title".into(), value("Test Song")),
            ("xesam:album".into(), value("Test Album")),
            (
                "xesam:artist".into(),
                value(vec!["Artist One", "Artist Two"]),
            ),
            (
                "mpris:artUrl".into(),
                value("https://example.test/cover.png"),
            ),
            ("xesam:url".into(), value("file:///music/song.flac")),
            ("mpris:length".into(), OwnedValue::from(180_000_000_i64)),
        ]);
        if !state.no_track {
            metadata.insert(
                "mpris:trackid".into(),
                value(ObjectPath::try_from("/track/one").unwrap()),
            );
        }
        metadata
    }
}

fn value<'a>(value: impl Into<Value<'a>>) -> OwnedValue {
    OwnedValue::try_from(value.into()).unwrap()
}

fn server(address: &str, name: &str, state: Arc<Mutex<State>>) -> Result<Connection> {
    Ok(Builder::address(address)?
        .serve_at(PATH, MockRoot(state.clone()))?
        .serve_at(PATH, MockPlayer(state))?
        .name(name)?
        .build()?)
}

fn properties(
    connection: &Connection,
    interface: &str,
    changed: Properties,
    invalidated: Vec<&str>,
) {
    connection
        .emit_signal(
            None::<&str>,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(interface, changed, invalidated),
        )
        .unwrap();
}

fn seeked(connection: &Connection, position: i64) {
    connection
        .emit_signal(None::<&str>, PATH, PLAYER, "Seeked", &(position,))
        .unwrap();
}

fn backend_on(address: &str) -> Result<MprisBackend> {
    let old_address = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address);
    let backend = MprisBackend::new();
    match old_address {
        Some(address) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address),
        None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
    }
    backend
}

fn wait_for(
    backend: &mut MprisBackend,
    predicate: impl Fn(&[PlaybackSnapshot]) -> bool,
) -> Vec<PlaybackSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if backend.changed() {
            let snapshots = backend.refresh().unwrap();
            if predicate(&snapshots) {
                return snapshots;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for MPRIS event"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn private_bus_mpris_lifecycle_commands_signals_and_cache() -> Result<()> {
    // One test owns this process's session-bus environment. No real desktop bus
    // or media player is touched, and no other test mutates this environment.
    let (bus, address) = Bus::start()?;
    let mut backend = backend_on(&address)?;
    assert!(backend.refresh()?.is_empty());
    assert!(!backend.changed());

    let state = Arc::new(Mutex::new(State {
        position: 12_000_000,
        volume: 0.6,
        ..State::default()
    }));
    let player = server(&address, NAME, state.clone())?;
    let snapshots = wait_for(&mut backend, |sources| sources.len() == 1);
    let source = &snapshots[0];
    assert_eq!(source.source_id, NAME);
    assert_eq!(source.source_name, "Mock Media Player");
    assert_eq!(source.track.id, "/track/one");
    assert_eq!(source.track.title, "Test Song");
    assert_eq!(source.track.artist, "Artist One, Artist Two");
    assert_eq!(source.track.album, "Test Album");
    assert_eq!(
        source.track.artwork_url.as_deref(),
        Some("https://example.test/cover.png")
    );
    assert_eq!(source.track.url.as_deref(), Some("file:///music/song.flac"));
    assert_eq!(source.track.duration, 180.0);
    assert_eq!(source.status, PlaybackStatus::Paused);
    assert_eq!(source.position, 12.0);
    assert_eq!(source.rate, 1.25);
    assert_eq!(source.volume, 0.6);
    assert!(source.capabilities.play && source.capabilities.pause && source.capabilities.seek);
    assert!(source.capabilities.next && source.capabilities.volume);
    assert!(!source.capabilities.previous);
    let reads = {
        let s = state.lock().unwrap();
        (s.identity_reads, s.metadata_reads, s.position_reads)
    };
    let timestamp = source.updated_at;
    for _ in 0..20 {
        assert_eq!(backend.refresh()?[0].updated_at, timestamp);
    }
    {
        let s = state.lock().unwrap();
        assert_eq!(
            (s.identity_reads, s.metadata_reads, s.position_reads),
            reads
        );
    }

    // Five-second service refreshes read only Position, even when paused.
    state.lock().unwrap().position = 13_000_000;
    thread::sleep(Duration::from_millis(5050));
    assert!(!backend.changed());
    assert_eq!(backend.refresh()?[0].position, 13.0);
    {
        let s = state.lock().unwrap();
        assert_eq!((s.identity_reads, s.metadata_reads), (reads.0, reads.1));
        assert_eq!(s.position_reads, reads.2 + 1);
    }

    properties(
        &player,
        ROOT,
        Properties::from([("Identity".into(), value("Renamed Player"))]),
        vec![],
    );
    wait_for(&mut backend, |s| s[0].source_name == "Renamed Player");
    properties(
        &player,
        PLAYER,
        Properties::from([
            ("PlaybackStatus".into(), value("Playing")),
            ("Rate".into(), value(2.0)),
            ("Volume".into(), value(0.75)),
        ]),
        vec![],
    );
    let snapshots = wait_for(&mut backend, |s| s[0].status == PlaybackStatus::Playing);
    assert_eq!(snapshots[0].rate, 2.0);
    assert_eq!(snapshots[0].volume, 0.75);
    seeked(&player, 45_000_000);
    wait_for(&mut backend, |s| s[0].position == 45.0);
    {
        let s = state.lock().unwrap();
        assert_eq!((s.identity_reads, s.metadata_reads), (reads.0, reads.1));
    }

    // Invalidated properties are re-read, rather than becoming defaults forever.
    properties(&player, PLAYER, Properties::new(), vec!["PlaybackStatus"]);
    wait_for(&mut backend, |s| s[0].status == PlaybackStatus::Paused);
    {
        let s = state.lock().unwrap();
        assert_eq!((s.identity_reads, s.metadata_reads), (reads.0, reads.1));
    }

    // PlayPause requires CanPause even when a player is currently paused.
    properties(
        &player,
        PLAYER,
        Properties::from([("CanPause".into(), value(false))]),
        vec![],
    );
    wait_for(&mut backend, |s| !s[0].capabilities.pause);
    assert!(backend.command(NAME, PlaybackCommand::PlayPause).is_err());
    assert!(backend.command(NAME, PlaybackCommand::Pause).is_err());
    properties(
        &player,
        PLAYER,
        Properties::from([("CanPause".into(), value(true))]),
        vec![],
    );
    wait_for(&mut backend, |s| s[0].capabilities.pause);
    properties(
        &player,
        PLAYER,
        Properties::from([("CanSeek".into(), value(false))]),
        vec![],
    );
    wait_for(&mut backend, |s| !s[0].capabilities.seek);
    assert!(backend.command(NAME, PlaybackCommand::Seek(1.0)).is_err());
    assert!(backend
        .command(NAME, PlaybackCommand::SetPosition(1.0))
        .is_err());
    properties(
        &player,
        PLAYER,
        Properties::from([("CanSeek".into(), value(true))]),
        vec![],
    );
    wait_for(&mut backend, |s| s[0].capabilities.seek);

    for command in [
        PlaybackCommand::Play,
        PlaybackCommand::Pause,
        PlaybackCommand::PlayPause,
        PlaybackCommand::Next,
    ] {
        backend.command(NAME, command)?;
    }
    assert!(backend.command(NAME, PlaybackCommand::Previous).is_err());
    backend.command(NAME, PlaybackCommand::Open("file:///test.ogg".into()))?;
    backend.command(NAME, PlaybackCommand::Seek(-2.0))?;
    backend.command(NAME, PlaybackCommand::SetPosition(30.0))?;
    assert!(state
        .lock()
        .unwrap()
        .calls
        .contains(&"SetPosition:/track/one:30000000".into()));
    backend.command(NAME, PlaybackCommand::Volume(1.4))?;
    assert_eq!(state.lock().unwrap().volume, 1.4);
    backend.command(NAME, PlaybackCommand::Volume(-1.0))?;
    assert_eq!(state.lock().unwrap().volume, 0.0);
    assert!(backend
        .command(NAME, PlaybackCommand::Seek(f64::NAN))
        .is_err());
    assert!(backend
        .command(NAME, PlaybackCommand::SetPosition(f64::INFINITY))
        .is_err());
    assert!(backend
        .command(NAME, PlaybackCommand::Volume(f64::NAN))
        .is_err());

    state.lock().unwrap().set_position_error = 1;
    backend.command(NAME, PlaybackCommand::SetPosition(40.0))?;
    assert_eq!(state.lock().unwrap().position, 40_000_000);
    assert_eq!(state.lock().unwrap().calls.last().unwrap(), "Seek:10000000");
    state.lock().unwrap().set_position_error = 2;
    let count = state.lock().unwrap().calls.len();
    assert!(backend
        .command(NAME, PlaybackCommand::SetPosition(50.0))
        .is_err());
    assert_eq!(
        state.lock().unwrap().calls.len(),
        count + 1,
        "real errors must not trigger fallback"
    );

    // A missing track id uses Seek; it never sends a string where D-Bus expects o.
    state.lock().unwrap().no_track = true;
    properties(&player, PLAYER, Properties::new(), vec!["Metadata"]);
    wait_for(&mut backend, |s| s[0].track.id.is_empty());
    backend.command(NAME, PlaybackCommand::SetPosition(50.0))?;
    assert_eq!(state.lock().unwrap().calls.last().unwrap(), "Seek:10000000");

    for (index, track_id) in [
        value("invalid object path"),
        value(ObjectPath::try_from(
            "/org/mpris/MediaPlayer2/TrackList/NoTrack",
        )?),
    ]
    .into_iter()
    .enumerate()
    {
        let title = format!("Fallback test {index}");
        let metadata = Properties::from([
            ("mpris:trackid".into(), track_id),
            ("xesam:title".into(), value(title.as_str())),
        ]);
        properties(
            &player,
            PLAYER,
            Properties::from([("Metadata".into(), OwnedValue::from(metadata))]),
            vec![],
        );
        wait_for(&mut backend, |s| s[0].track.title == title);
        backend.command(NAME, PlaybackCommand::SetPosition(50.0))?;
        assert_eq!(state.lock().unwrap().calls.last().unwrap(), "Seek:0");
    }
    properties(
        &player,
        ROOT,
        Properties::from([("SupportedUriSchemes".into(), value(vec!["file"]))]),
        vec![],
    );
    seeked(&player, 50_000_000);
    wait_for(&mut backend, |s| s[0].position == 50.0);
    assert!(backend
        .command(NAME, PlaybackCommand::Open("https://example.test/a".into()))
        .is_err());

    properties(
        &player,
        PLAYER,
        Properties::from([("CanControl".into(), value(false))]),
        vec![],
    );
    wait_for(&mut backend, |s| !s[0].capabilities.play);
    let count = state.lock().unwrap().calls.len();
    for command in [
        PlaybackCommand::Play,
        PlaybackCommand::Pause,
        PlaybackCommand::PlayPause,
        PlaybackCommand::Next,
        PlaybackCommand::Previous,
        PlaybackCommand::Seek(1.0),
        PlaybackCommand::SetPosition(1.0),
        PlaybackCommand::Volume(1.0),
        PlaybackCommand::Open("file:///x".into()),
    ] {
        assert!(backend.command(NAME, command).is_err());
    }
    assert_eq!(state.lock().unwrap().calls.len(), count);

    // Another owner's lookalike signals cannot modify a genuine player.
    let unrelated = Builder::address(address.as_str())?.build()?;
    unrelated.request_name("org.example.UnrelatedService")?;
    properties(
        &unrelated,
        PLAYER,
        Properties::from([("PlaybackStatus".into(), value("Stopped"))]),
        vec![],
    );
    seeked(&unrelated, 99_000_000);
    seeked(&player, 51_000_000);
    let sources = wait_for(&mut backend, |s| s[0].position == 51.0);
    assert_eq!(sources[0].status, PlaybackStatus::Paused);

    let other_state = Arc::new(Mutex::new(State::default()));
    let other = server(&address, "org.mpris.MediaPlayer2.SecondTest", other_state)?;
    wait_for(&mut backend, |s| s.len() == 2);
    player.close()?;
    wait_for(&mut backend, |s| s.len() == 1 && s[0].source_id != NAME);
    assert!(backend.command(NAME, PlaybackCommand::Play).is_err());
    let replacement = server(&address, NAME, state)?;
    wait_for(&mut backend, |s| s.len() == 2);
    replacement.close()?;
    other.close()?;
    wait_for(&mut backend, |s| s.is_empty());
    let mut disconnected_backend = backend_on(&address)?;
    disconnected_backend.refresh()?;
    let start = Instant::now();
    drop(backend);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "drop must wake and join idle workers"
    );
    drop(bus);
    wait_for(&mut disconnected_backend, |s| s.is_empty());
    assert!(disconnected_backend
        .command(NAME, PlaybackCommand::Play)
        .is_err());
    Ok(())
}
