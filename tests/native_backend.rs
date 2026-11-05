#![cfg(unix)]

use lyrictty::playback::{
    native::{NativeBackend, NativeOptions},
    PlaybackBackend, PlaybackCommand, PlaybackSnapshot, PlaybackStatus, Track,
};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

// The fake executable speaks actual newline-delimited JSON over a Unix socket.
// No direct access to backend internals, extractor, network, or account is used.
const MOCK_MPV: &str = r#"#!/usr/bin/env python3
import json, os, socket, sys, time
args = sys.argv[1:]
directory = next(a.split('=', 1)[1] for a in args if a.startswith('--mock-directory='))
scenario = next(a.split('=', 1)[1] for a in args if a.startswith('--mock-scenario='))
path = next(a.split('=', 1)[1] for a in args if a.startswith('--input-ipc-server='))
open(directory + '/pid', 'w').write(str(os.getpid()))
open(directory + '/socket', 'w').write(path)
json.dump(args, open(directory + '/args', 'w'))
if scenario == 'no-socket':
    time.sleep(60)
    sys.exit(0)
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen(1)
conn, _ = server.accept()
reader = conn.makefile('rb')
observed = set()
props = {'idle-active': True, 'pause': False, 'core-idle': True,
         'volume': 100, 'speed': 1, 'playlist-pos': -1, 'playlist-count': 0}
late = None
def send(message):
    data = (json.dumps(message, ensure_ascii=False) + '\n').encode()
    # Replies and events can be fragmented anywhere, not just between lines.
    conn.sendall(data[:5])
    conn.sendall(data[5:])
def reply(request, data=None, error='success'):
    send({'request_id': request['request_id'], 'error': error, 'data': data})
def prop(name, value):
    unchanged = name in props and props[name] == value
    props[name] = value
    if scenario == 'coalesced' and unchanged:
        return
    if name in observed:
        send({'event': 'property-change', 'name': name, 'data': value})
def load(path, index=0, start=True):
    if start:
        send({'event': 'start-file', 'playlist_entry_id': index + 1})
    prop('idle-active', False)
    prop('path', path)
    prop('metadata', {'TITLE': 'Tagged title ' + str(index), 'Artist': 'Test artist', 'ALBUM': 'Test album'})
    prop('media-title', 'Fallback title')
    prop('duration', 120)
    prop('time-pos', 0)
    prop('seekable', 'unseekable' not in path)
    prop('pause', False)
    prop('core-idle', False)
    prop('playlist-pos', index)
    prop('playlist-count', 3)
    if 'first' in path:
        prop('user-data/mpv/ytdl/json-subprocess-result', {'status': 0, 'stdout': json.dumps({
            'thumbnail': 'https://art.invalid/first.jpg', 'title': 'Extractor title'})})
    else:
        prop('user-data/mpv/ytdl/json-subprocess-result', None)
    send({'event': 'file-loaded'})
print('stdout deliberately is not an IPC protocol', flush=True)
for line in reader:
    request = json.loads(line)
    with open(directory + '/commands', 'a') as log:
        log.write(json.dumps(request) + '\n')
    command = request['command']
    name = command[0]
    if name == 'observe_property':
        observed.add(command[2])
        reply(request)
        prop(command[2], props.get(command[2]))
    elif name == 'loadfile':
        # Wrong IDs, including errors, must never satisfy a pending request.
        send({'request_id': request['request_id'] + 10000, 'error': 'unrelated failure'})
        load(command[1], start=scenario != 'path-only')
        reply(request)
    elif name == 'get_property':
        if scenario == 'missing-properties' and command[1] in ('metadata', 'duration', 'user-data/mpv/ytdl/json-subprocess-result'):
            reply(request, error='property unavailable')
            continue
        reply(request, props.get(command[1]))
    elif name == 'set_property' and command[1] == 'volume':
        volume = round(command[2])
        if scenario == 'timeout' and volume == 11:
            late = request
            continue
        if late:
            reply(late, error='late failure from timed-out request')
            late = None
        if scenario == 'error' and volume == 13:
            reply(request, error='property unavailable')
            continue
        if scenario == 'disconnect' and volume == 33:
            conn.shutdown(socket.SHUT_RDWR)
            break
        if scenario == 'malformed' and volume == 44:
            conn.sendall(b'{invalid json}\n')
            continue
        if scenario == 'events' and volume == 66:
            prop('metadata', {'title': 'Live title', 'artist': 'Live artist', 'album': 'Live album',
                              'artwork_url': 'https://art.invalid/live.jpg'})
            prop('speed', 1.5)
            prop('time-pos', 21)
        if scenario == 'events' and volume == 77:
            send({'event': 'end-file', 'reason': 'eof'})
            prop('idle-active', True)
            prop('playlist-pos', -1)
        if scenario == 'events' and volume == 88:
            prop('eof-reached', True)
        prop('volume', command[2])
        reply(request)
    elif name == 'set_property':
        prop(command[1], command[2])
        reply(request)
    elif name == 'cycle':
        prop('pause', not props.get('pause', False))
        reply(request)
    elif name == 'seek':
        prop('time-pos', command[1])
        send({'event': 'seek'})
        reply(request)
    elif name in ('playlist-next', 'playlist-prev'):
        index = props['playlist-pos'] + (1 if name == 'playlist-next' else -1)
        load('mock:playlist-' + str(index), index)
        reply(request)
    elif name == 'quit':
        if scenario == 'ignore-quit':
            time.sleep(60)
        break
    else:
        reply(request, error='command not found')
"#;

struct Mock {
    directory: TempDir,
    backend: NativeBackend,
}

impl Mock {
    fn new(scenario: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("mock-mpv.py");
        fs::write(&executable, MOCK_MPV).unwrap();
        // Execute a stable interpreter, not a file another parallel fork may
        // briefly have inherited open for writing (Linux ETXTBSY).
        let backend = NativeBackend::with_options(NativeOptions {
            mpv_path: "python3".into(),
            extra_args: vec![
                executable.into_os_string(),
                format!("--mock-directory={}", directory.path().display()).into(),
                format!("--mock-scenario={scenario}").into(),
            ],
            startup_timeout: Duration::from_millis(800),
            command_timeout: Duration::from_millis(150),
        });
        Self { directory, backend }
    }

    fn open(&mut self, path: &str) {
        self.backend
            .command("native", PlaybackCommand::Open(path.into()))
            .unwrap();
        wait_snapshot(&mut self.backend, |s| s.status == PlaybackStatus::Playing);
    }

    fn commands(&self) -> Vec<Value> {
        fs::read_to_string(self.directory.path().join("commands"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn marker(&self, name: &str) -> String {
        fs::read_to_string(self.directory.path().join(name)).unwrap()
    }
}

#[test]
fn unavailable_optional_file_properties_do_not_disconnect_playback() {
    let mut mock = Mock::new("missing-properties");
    mock.open("mock:stream");
    let state = snapshot(&mut mock.backend);
    assert_eq!(state.status, PlaybackStatus::Playing);
    mock.backend
        .command("native", PlaybackCommand::Pause)
        .unwrap();
    wait_snapshot(&mut mock.backend, |s| s.status == PlaybackStatus::Paused);
}

fn snapshot(backend: &mut NativeBackend) -> PlaybackSnapshot {
    let mut sources = backend.refresh().unwrap();
    assert_eq!(sources.len(), 1);
    sources.remove(0)
}

fn wait_snapshot(
    backend: &mut NativeBackend,
    predicate: impl Fn(&PlaybackSnapshot) -> bool,
) -> PlaybackSnapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = snapshot(backend);
        if predicate(&value) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "snapshot did not settle: {value:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn construction_and_idle_refresh_are_lazy_and_native_is_visible() {
    let mut backend = NativeBackend::with_options(NativeOptions {
        mpv_path: "/does/not/exist/mpv".into(),
        ..NativeOptions::default()
    });
    let idle = snapshot(&mut backend);
    assert_eq!(idle.source_id, "native");
    assert_eq!(idle.source_name, "mpv");
    assert_eq!(idle.status, PlaybackStatus::Stopped);
    assert_eq!(idle.track, Track::default());
    assert!(!idle.capabilities.play && !idle.capabilities.pause && !idle.capabilities.seek);
    assert!(idle.capabilities.volume);
    assert!(!backend.changed());
    assert!(backend
        .command("other", PlaybackCommand::Open("file.wav".into()))
        .is_err());
    assert!(backend
        .command("native", PlaybackCommand::Open("  ".into()))
        .is_err());
    assert!(backend
        .command("native", PlaybackCommand::Open("a\0b".into()))
        .is_err());
    assert!(backend.command("native", PlaybackCommand::Play).is_err());
    assert!(backend
        .command("native", PlaybackCommand::Volume(f64::NAN))
        .is_err());
    assert!(backend
        .command("native", PlaybackCommand::Open("file.wav".into()))
        .is_err());
    assert_eq!(snapshot(&mut backend).status, PlaybackStatus::Stopped);
    let _default_constructor: NativeBackend = NativeBackend::new();
}

#[test]
fn ipc_correlates_replies_observes_metadata_and_refresh_never_polls() {
    let mut mock = Mock::new("normal");
    let path = "https://example.invalid/first?x=\"quoted\"&u=音楽\nnext";
    mock.open(path);
    let state = snapshot(&mut mock.backend);
    assert_eq!(state.track.url.as_deref(), Some(path));
    assert_eq!(state.track.title, "Tagged title 0");
    assert_eq!(state.track.artist, "Test artist");
    assert_eq!(state.track.album, "Test album");
    assert_eq!(state.track.duration, 120.0);
    assert_eq!(
        state.track.artwork_url.as_deref(),
        Some("https://art.invalid/first.jpg")
    );
    assert!(state.capabilities.pause && state.capabilities.seek && state.capabilities.next);
    assert!(!state.capabilities.previous);
    assert!(mock.backend.changed());
    assert!(!mock.backend.changed());
    let commands = mock.commands();
    assert!(commands
        .iter()
        .all(|c| c["request_id"].as_u64().is_some_and(|id| id > 0)));
    assert!(commands
        .iter()
        .any(|c| c["command"] == json!(["loadfile", path, "replace"])));
    for _ in 0..100 {
        let _ = snapshot(&mut mock.backend);
    }
    assert_eq!(
        commands,
        mock.commands(),
        "snapshot must not send IPC or run yt-dlp"
    );
    let args: Vec<String> = serde_json::from_str(&mock.marker("args")).unwrap();
    for arg in [
        "--idle=yes",
        "--no-video",
        "--ytdl=yes",
        "--script-opts=ytdl_hook-ytdl_path=yt-dlp",
    ] {
        assert!(args.iter().any(|a| a == arg));
    }
    assert!(!args.iter().any(|a| a == "--ao=null"));
}

#[test]
fn pause_position_volume_seek_validation_and_playlist_bounds() {
    let mut mock = Mock::new("normal");
    mock.open("mock:first");
    mock.backend
        .command("native", PlaybackCommand::Pause)
        .unwrap();
    let paused = wait_snapshot(&mut mock.backend, |s| s.status == PlaybackStatus::Paused);
    thread::sleep(Duration::from_millis(30));
    assert_eq!(
        paused.position_now(),
        snapshot(&mut mock.backend).position_now()
    );
    mock.backend
        .command("native", PlaybackCommand::SetPosition(20.0))
        .unwrap();
    assert_eq!(snapshot(&mut mock.backend).position, 20.0);
    mock.backend
        .command("native", PlaybackCommand::Seek(-100.0))
        .unwrap();
    assert_eq!(snapshot(&mut mock.backend).position, 0.0);
    mock.backend
        .command("native", PlaybackCommand::SetPosition(1000.0))
        .unwrap();
    assert_eq!(snapshot(&mut mock.backend).position, 120.0);
    assert!(mock
        .backend
        .command("native", PlaybackCommand::Seek(f64::INFINITY))
        .is_err());
    mock.backend
        .command("native", PlaybackCommand::Volume(0.42))
        .unwrap();
    assert_eq!(snapshot(&mut mock.backend).volume, 0.42);
    mock.backend
        .command("native", PlaybackCommand::Volume(2.0))
        .unwrap();
    assert_eq!(snapshot(&mut mock.backend).volume, 1.0);
    mock.backend
        .command("native", PlaybackCommand::PlayPause)
        .unwrap();
    wait_snapshot(&mut mock.backend, |s| s.status == PlaybackStatus::Playing);
    mock.backend
        .command("native", PlaybackCommand::Play)
        .unwrap();
    mock.backend
        .command("native", PlaybackCommand::Previous)
        .unwrap();
    assert!(!mock
        .commands()
        .iter()
        .any(|c| c["command"][0] == "playlist-prev"));
    mock.backend
        .command("native", PlaybackCommand::Next)
        .unwrap();
    let second = wait_snapshot(&mut mock.backend, |s| {
        s.track.url.as_deref() == Some("mock:playlist-1")
    });
    assert!(second.capabilities.next && second.capabilities.previous);
    assert_eq!(second.track.artwork_url, None);
    mock.backend
        .command("native", PlaybackCommand::Next)
        .unwrap();
    assert!(!snapshot(&mut mock.backend).capabilities.next);
    let count = mock
        .commands()
        .iter()
        .filter(|c| c["command"][0] == "playlist-next")
        .count();
    mock.backend
        .command("native", PlaybackCommand::Next)
        .unwrap();
    assert_eq!(
        count,
        mock.commands()
            .iter()
            .filter(|c| c["command"][0] == "playlist-next")
            .count()
    );
    mock.backend
        .command("native", PlaybackCommand::Previous)
        .unwrap();
    assert_eq!(
        snapshot(&mut mock.backend).track.url.as_deref(),
        Some("mock:playlist-1")
    );
    mock.open("mock:unseekable");
    assert!(!snapshot(&mut mock.backend).capabilities.seek);
    assert!(mock
        .backend
        .command("native", PlaybackCommand::SetPosition(10.0))
        .is_err());
}

#[test]
fn track_change_without_start_event_clears_artwork_and_old_position() {
    let mut mock = Mock::new("path-only");
    mock.open("mock:first");
    mock.backend
        .command("native", PlaybackCommand::SetPosition(50.0))
        .unwrap();
    mock.open("mock:second");
    let state = snapshot(&mut mock.backend);
    assert_eq!(state.track.url.as_deref(), Some("mock:second"));
    assert_eq!(state.track.artwork_url, None);
    assert!(state.position_now() < 1.0);
}

#[test]
fn identical_properties_are_reseeded_once_when_observations_coalesce() {
    let mut mock = Mock::new("coalesced");
    mock.open("mock:first");
    mock.open("mock:first");
    let same = snapshot(&mut mock.backend);
    assert_eq!(same.track.title, "Tagged title 0");
    assert_eq!(same.track.artist, "Test artist");
    assert_eq!(same.track.album, "Test album");
    assert_eq!(same.track.duration, 120.0);
    assert!(same.capabilities.seek);
    assert_eq!(
        same.track.artwork_url.as_deref(),
        Some("https://art.invalid/first.jpg")
    );
    mock.open("mock:second");
    let other = snapshot(&mut mock.backend);
    assert_eq!(other.track.title, "Tagged title 0");
    assert_eq!(other.track.duration, 120.0);
    assert!(other.capabilities.seek);
    assert_eq!(other.track.artwork_url, None);
    let commands = mock.commands();
    for _ in 0..20 {
        let _ = snapshot(&mut mock.backend);
    }
    assert_eq!(commands, mock.commands());
}

#[test]
fn property_events_update_live_tags_rate_and_end_of_file() {
    let mut mock = Mock::new("events");
    mock.open("mock:first");
    mock.backend.changed();
    mock.backend
        .command("native", PlaybackCommand::Volume(0.66))
        .unwrap();
    assert!(mock.backend.changed());
    let state = snapshot(&mut mock.backend);
    assert_eq!(state.track.title, "Live title");
    assert_eq!(state.track.artist, "Live artist");
    assert_eq!(state.track.album, "Live album");
    assert_eq!(
        state.track.artwork_url.as_deref(),
        Some("https://art.invalid/live.jpg")
    );
    assert_eq!(state.rate, 1.5);
    assert!((21.0..22.0).contains(&state.position_now()));
    thread::sleep(Duration::from_millis(30));
    assert!(snapshot(&mut mock.backend).position_now() > state.position_now() - 0.01);
    mock.backend
        .command("native", PlaybackCommand::Volume(0.88))
        .unwrap();
    let eof = snapshot(&mut mock.backend);
    assert_eq!(eof.status, PlaybackStatus::Stopped);
    assert!(!eof.capabilities.pause && !eof.capabilities.seek);
    mock.backend
        .command("native", PlaybackCommand::Volume(0.77))
        .unwrap();
    let stopped = snapshot(&mut mock.backend);
    assert_eq!(stopped.status, PlaybackStatus::Stopped);
    assert_eq!(stopped.track, Track::default());
    assert_eq!(stopped.position, 0.0);
    assert!(!stopped.capabilities.pause && !stopped.capabilities.seek);
}

#[test]
fn mpv_error_is_reported_without_optimistic_state_change() {
    let mut mock = Mock::new("error");
    mock.open("mock:first");
    let error = mock
        .backend
        .command("native", PlaybackCommand::Volume(0.13))
        .unwrap_err();
    assert!(error.to_string().contains("property unavailable"));
    assert_eq!(snapshot(&mut mock.backend).volume, 1.0);
    mock.backend
        .command("native", PlaybackCommand::Volume(0.42))
        .unwrap();
    assert_eq!(snapshot(&mut mock.backend).volume, 0.42);
}

#[test]
fn timeout_is_bounded_and_late_reply_cannot_poison_next_request() {
    let mut mock = Mock::new("timeout");
    mock.open("mock:first");
    let start = Instant::now();
    assert!(mock
        .backend
        .command("native", PlaybackCommand::Volume(0.11))
        .is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    mock.backend
        .command("native", PlaybackCommand::Volume(0.22))
        .unwrap();
    assert_eq!(snapshot(&mut mock.backend).volume, 0.22);
}

#[test]
fn disconnect_and_malformed_protocol_wake_waiters_and_allow_open_to_restart() {
    for (scenario, volume) in [("disconnect", 0.33), ("malformed", 0.44)] {
        let mut mock = Mock::new(scenario);
        mock.open("mock:first");
        let pid = mock.marker("pid");
        let start = Instant::now();
        assert!(mock
            .backend
            .command("native", PlaybackCommand::Volume(volume))
            .is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(snapshot(&mut mock.backend).status, PlaybackStatus::Stopped);
        assert!(mock.backend.changed());
        mock.open("mock:second");
        assert_ne!(pid, mock.marker("pid"));
    }
}

#[test]
fn private_socket_and_stubborn_child_are_cleaned_on_drop() {
    let mut mock = Mock::new("ignore-quit");
    mock.open("mock:first");
    let path = PathBuf::from(mock.marker("socket"));
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let pid = mock.marker("pid");
    let start = Instant::now();
    drop(mock.backend);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!path.parent().unwrap().exists());
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new("/proc").join(pid).exists(),
        "child must be reaped"
    );
}

#[test]
fn failed_startup_reaps_child_and_removes_private_directory() {
    let mut mock = Mock::new("no-socket");
    let start = Instant::now();
    assert!(mock
        .backend
        .command("native", PlaybackCommand::Open("file.wav".into()))
        .is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    let socket = PathBuf::from(mock.marker("socket"));
    assert!(!socket.parent().unwrap().exists());
    #[cfg(target_os = "linux")]
    assert!(!Path::new("/proc").join(mock.marker("pid")).exists());
}

fn wav(path: &Path, title: &str) {
    let mut info = b"INFO".to_vec();
    for (tag, value) in [
        (b"INAM", title),
        (b"IART", "WAV artist"),
        (b"IPRD", "WAV album"),
    ] {
        info.extend(tag);
        let len = value.len() + 1;
        info.extend((len as u32).to_le_bytes());
        info.extend(value.as_bytes());
        info.push(0);
        if len % 2 != 0 {
            info.push(0);
        }
    }
    let samples = 8000u32 * 30;
    let data_size = samples * 2;
    let mut file = fs::File::create(path).unwrap();
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + data_size + 8 + info.len() as u32).to_le_bytes())
        .unwrap();
    file.write_all(b"WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00")
        .unwrap();
    file.write_all(&8000u32.to_le_bytes()).unwrap();
    file.write_all(&16000u32.to_le_bytes()).unwrap();
    file.write_all(b"\x02\x00\x10\x00LIST").unwrap();
    file.write_all(&(info.len() as u32).to_le_bytes()).unwrap();
    file.write_all(&info).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&data_size.to_le_bytes()).unwrap();
    file.write_all(&vec![0u8; data_size as usize]).unwrap();
}

fn real_backend() -> Option<NativeBackend> {
    if Command::new("mpv").arg("--version").output().is_err() {
        eprintln!("skipping headless playback test: mpv is not installed");
        return None;
    }
    Some(NativeBackend::with_options(NativeOptions {
        extra_args: vec!["--ao=null".into()],
        ..NativeOptions::default()
    }))
}

#[test]
fn real_headless_mpv_plays_tagged_wav_pauses_seeks_and_replaces_track() {
    let Some(mut backend) = real_backend() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first 音楽 with spaces.wav");
    let second = directory.path().join("second.wav");
    wav(&first, "First WAV");
    wav(&second, "Second WAV");
    backend
        .command(
            "native",
            PlaybackCommand::Open(first.to_str().unwrap().into()),
        )
        .unwrap();
    let playing = wait_snapshot(&mut backend, |s| {
        s.status == PlaybackStatus::Playing && s.track.title == "First WAV" && s.capabilities.seek
    });
    assert_eq!(playing.track.url.as_deref(), first.to_str());
    assert_eq!(playing.track.artist, "WAV artist");
    assert_eq!(playing.track.album, "WAV album");
    assert!((playing.track.duration - 30.0).abs() < 0.1);
    assert!(!playing.capabilities.next && !playing.capabilities.previous);
    backend.command("native", PlaybackCommand::Pause).unwrap();
    let paused = wait_snapshot(&mut backend, |s| s.status == PlaybackStatus::Paused);
    thread::sleep(Duration::from_millis(80));
    assert!((paused.position_now() - snapshot(&mut backend).position_now()).abs() < 0.1);
    backend
        .command("native", PlaybackCommand::SetPosition(10.0))
        .unwrap();
    wait_snapshot(&mut backend, |s| (s.position - 10.0).abs() < 0.15);
    backend
        .command("native", PlaybackCommand::Seek(-2.0))
        .unwrap();
    wait_snapshot(&mut backend, |s| (s.position - 8.0).abs() < 0.15);
    backend
        .command("native", PlaybackCommand::Volume(0.37))
        .unwrap();
    wait_snapshot(&mut backend, |s| (s.volume - 0.37).abs() < 0.001);
    backend
        .command("native", PlaybackCommand::PlayPause)
        .unwrap();
    wait_snapshot(&mut backend, |s| s.status == PlaybackStatus::Playing);
    backend.command("native", PlaybackCommand::Next).unwrap();
    backend
        .command("native", PlaybackCommand::Previous)
        .unwrap();
    assert_eq!(snapshot(&mut backend).track.id, playing.track.id);
    backend
        .command(
            "native",
            PlaybackCommand::Open(second.to_str().unwrap().into()),
        )
        .unwrap();
    let next = wait_snapshot(&mut backend, |s| {
        s.track.title == "Second WAV" && s.status == PlaybackStatus::Playing
    });
    assert_ne!(next.track.id, playing.track.id);
    assert_eq!(next.track.artwork_url, None);
    assert!(next.capabilities.seek);
    assert!((next.track.duration - 30.0).abs() < 0.1);
    assert!(next.position_now() < 2.0);
    let start = Instant::now();
    drop(backend);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn real_headless_mpv_respects_both_playlist_boundaries() {
    let Some(mut backend) = real_backend() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("one.wav");
    let second = directory.path().join("two.wav");
    wav(&first, "One");
    wav(&second, "Two");
    let playlist = directory.path().join("local.m3u");
    fs::write(
        &playlist,
        format!("#EXTM3U\n{}\n{}\n", first.display(), second.display()),
    )
    .unwrap();
    backend
        .command(
            "native",
            PlaybackCommand::Open(playlist.to_str().unwrap().into()),
        )
        .unwrap();
    let first_state = wait_snapshot(&mut backend, |s| {
        s.track.title == "One" && s.capabilities.next
    });
    assert!(!first_state.capabilities.previous);
    backend
        .command("native", PlaybackCommand::Previous)
        .unwrap();
    assert_eq!(snapshot(&mut backend).track.title, "One");
    backend.command("native", PlaybackCommand::Next).unwrap();
    let second_state = wait_snapshot(&mut backend, |s| {
        s.track.title == "Two" && s.capabilities.previous
    });
    assert!(!second_state.capabilities.next);
    backend.command("native", PlaybackCommand::Next).unwrap();
    assert_eq!(snapshot(&mut backend).track.title, "Two");
    backend
        .command("native", PlaybackCommand::Previous)
        .unwrap();
    wait_snapshot(&mut backend, |s| {
        s.track.title == "One" && s.status == PlaybackStatus::Playing
    });
}
