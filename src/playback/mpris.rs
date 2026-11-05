//! Direct session-bus MPRIS backend. No player-specific processes or polling loop.

use super::{
    Capabilities, PlaybackBackend, PlaybackCommand, PlaybackSnapshot, PlaybackStatus, Track,
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zbus::{
    blocking::{connection::Builder, fdo::DBusProxy, Connection, MessageIterator, Proxy},
    message::{Message, Type},
    proxy::CacheProperties,
    zvariant::{ObjectPath, OwnedValue},
    MatchRule,
};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const TRACK_LIST: &str = "org.mpris.MediaPlayer2.TrackList";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const PATH: &str = "/org/mpris/MediaPlayer2";
const RESYNC_INTERVAL: Duration = Duration::from_secs(15);
const POSITION_INTERVAL: Duration = Duration::from_secs(5);
type Properties = HashMap<String, OwnedValue>;

enum Event {
    Owners(String),
    Properties(String, String, Properties, Vec<String>),
    Seeked(String, i64),
    Resync,
    Disconnected,
}

struct Player {
    owner: String,
    root: Properties,
    properties: Properties,
    snapshot: PlaybackSnapshot,
    invalid_root: BTreeSet<String>,
    invalid_player: BTreeSet<String>,
    resync_position: bool,
    position_synced_at: Instant,
}

/// A cached, event-driven backend for all currently running MPRIS players.
///
/// Construction requires a session bus. Player failures are isolated from other
/// sources; command failures are returned to the caller. Dropping the backend
/// closes its private connection and joins both background workers.
pub struct MprisBackend {
    connection: Connection,
    events: Receiver<Event>,
    players: BTreeMap<String, Player>,
    discover: bool,
    dirty: bool,
    disconnected: bool,
    signal_worker: Option<JoinHandle<()>>,
    timer_worker: Option<JoinHandle<()>>,
    timer_stop: Sender<()>,
}

impl MprisBackend {
    pub fn new() -> Result<Self> {
        let connection = Builder::session()?
            .method_timeout(Duration::from_secs(2))
            .max_queued(1024)
            .build()
            .context("connect to the MPRIS session bus")?;
        // Install the receiver before the match rules and initial discovery, so
        // players appearing during construction cannot be missed.
        let iterator = MessageIterator::from(&connection);
        let bus = DBusProxy::new(&connection)?;
        bus.add_match_rule(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")?
                .path("/org/freedesktop/DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .arg0ns(ROOT)?
                .build(),
        )?;
        for interface in [ROOT, PLAYER] {
            bus.add_match_rule(
                MatchRule::builder()
                    .msg_type(Type::Signal)
                    .path(PATH)?
                    .interface(PROPERTIES)?
                    .member("PropertiesChanged")?
                    .arg(0, interface)?
                    .build(),
            )?;
        }
        bus.add_match_rule(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .path(PATH)?
                .interface(PLAYER)?
                .member("Seeked")?
                .build(),
        )?;
        drop(bus);

        let (sender, events) = mpsc::channel();
        let (timer_stop, stop) = mpsc::channel();
        let signal_sender = sender.clone();
        let signal_worker =
            thread::Builder::new()
                .name("mpris-signals".into())
                .spawn(move || {
                    for message in iterator {
                        match message {
                            Ok(message) => {
                                if let Some(event) = signal_event(&message) {
                                    if signal_sender.send(event).is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let _ = signal_sender.send(Event::Disconnected);
                })?;
        let mut backend = Self {
            connection,
            events,
            players: BTreeMap::new(),
            discover: true,
            dirty: true,
            disconnected: false,
            signal_worker: Some(signal_worker),
            timer_worker: None,
            timer_stop,
        };
        backend.timer_worker = Some(
            thread::Builder::new()
                .name("mpris-position-resync".into())
                .spawn(move || {
                    while let Err(mpsc::RecvTimeoutError::Timeout) =
                        stop.recv_timeout(RESYNC_INTERVAL)
                    {
                        if sender.send(Event::Resync).is_err() {
                            break;
                        }
                    }
                })?,
        );
        Ok(backend)
    }

    fn receive_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Owners(name) => {
                    self.players.remove(&name);
                    self.discover = true;
                    self.dirty = true;
                }
                Event::Disconnected => {
                    self.disconnected = true;
                    self.players.clear();
                    self.dirty = true;
                }
                Event::Resync => {
                    for player in self.players.values_mut() {
                        if player.snapshot.status == PlaybackStatus::Playing
                            && player.position_synced_at.elapsed() >= POSITION_INTERVAL
                        {
                            player.resync_position = true;
                            self.dirty = true;
                        }
                    }
                }
                Event::Seeked(owner, position) => {
                    for player in self.players.values_mut().filter(|p| p.owner == owner) {
                        player.snapshot.position = if player.snapshot.track.duration > 0.0 {
                            seconds(position).min(player.snapshot.track.duration)
                        } else {
                            seconds(position)
                        };
                        player.snapshot.updated_at = Instant::now();
                        player.position_synced_at = Instant::now();
                        self.dirty = true;
                    }
                }
                Event::Properties(owner, interface, changed, invalidated) => {
                    for player in self.players.values_mut().filter(|p| p.owner == owner) {
                        // Preserve extrapolated position when rate/status changes.
                        let position = player.snapshot.position_now();
                        let properties = if interface == ROOT {
                            &mut player.root
                        } else {
                            &mut player.properties
                        };
                        for key in &invalidated {
                            properties.remove(key);
                        }
                        for (key, value) in &changed {
                            if let Ok(value) = value.try_clone() {
                                properties.insert(key.clone(), value);
                            }
                        }
                        if interface == ROOT {
                            player.invalid_root.extend(invalidated.iter().cloned());
                            player.invalid_root.retain(|key| !changed.contains_key(key));
                        } else {
                            player.invalid_player.extend(invalidated.iter().cloned());
                            player
                                .invalid_player
                                .retain(|key| !changed.contains_key(key));
                            player.resync_position |= changed.contains_key("Metadata")
                                || invalidated.iter().any(|key| key == "Metadata");
                        }
                        let position = changed
                            .get("Position")
                            .and_then(|v| i64::try_from(v).ok())
                            .map(seconds)
                            .unwrap_or(position);
                        let mut next = snapshot(
                            &player.snapshot.source_id,
                            &player.root,
                            &player.properties,
                            position,
                        );
                        if next.track.id != player.snapshot.track.id
                            || (player.snapshot.status == PlaybackStatus::Stopped
                                && next.status == PlaybackStatus::Playing)
                        {
                            next.position = 0.0;
                            player.resync_position = true;
                        }
                        player.snapshot = next;
                        self.dirty = true;
                    }
                }
            }
        }
    }

    fn discover_players(&mut self) -> Result<()> {
        let bus = DBusProxy::new(&self.connection)?;
        let names = bus.list_names()?;
        self.players
            .retain(|name, _| names.iter().any(|n| n.as_str() == name));
        for name in names
            .iter()
            .filter(|name| name.as_str().starts_with(PREFIX))
        {
            let Ok(owner) = bus.get_name_owner(name.as_ref()) else {
                self.players.remove(name.as_str());
                continue;
            };
            if self
                .players
                .get(name.as_str())
                .is_some_and(|player| player.owner == owner.as_str())
            {
                continue;
            }
            self.players.remove(name.as_str());
            // Address the unique owner so a replacement cannot mix two players'
            // properties or receive a command meant for its predecessor.
            let loaded = (|| -> Result<Player> {
                let root = get_all(&self.connection, owner.as_str(), ROOT)?;
                let properties = get_all(&self.connection, owner.as_str(), PLAYER)?;
                let position = integer(&properties, "Position").map(seconds).unwrap_or(0.0);
                let snapshot = snapshot(name.as_str(), &root, &properties, position);
                Ok(Player {
                    owner: owner.to_string(),
                    root,
                    properties,
                    snapshot,
                    invalid_root: BTreeSet::new(),
                    invalid_player: BTreeSet::new(),
                    resync_position: false,
                    position_synced_at: Instant::now(),
                })
            })();
            // A disappearing or nonconforming player must not hide healthy ones.
            if let Ok(player) = loaded {
                self.players.insert(name.to_string(), player);
            }
        }
        Ok(())
    }

    fn execute(&self, source_id: &str, command: PlaybackCommand) -> Result<()> {
        let player = self
            .players
            .get(source_id)
            .context("MPRIS source disconnected")?;
        let capabilities = &player.snapshot.capabilities;
        let proxy = proxy(&self.connection, &player.owner, PLAYER)?;
        let allowed = match &command {
            PlaybackCommand::Play => capabilities.play,
            PlaybackCommand::Pause => capabilities.pause,
            PlaybackCommand::PlayPause => {
                capabilities.pause
                    && (player.snapshot.status == PlaybackStatus::Playing || capabilities.play)
            }
            PlaybackCommand::Next => capabilities.next,
            PlaybackCommand::Previous => capabilities.previous,
            PlaybackCommand::Repeat => {
                boolean(&player.properties, "CanControl")
                    && player.properties.contains_key("LoopStatus")
            }
            PlaybackCommand::Shuffle => {
                boolean(&player.properties, "CanControl")
                    && player.properties.contains_key("Shuffle")
            }
            PlaybackCommand::Seek(_) | PlaybackCommand::SetPosition(_) => capabilities.seek,
            PlaybackCommand::Volume(_) => capabilities.volume,
            PlaybackCommand::Open(_) => boolean(&player.properties, "CanControl"),
        };
        ensure!(allowed, "MPRIS source does not support this command");
        match command {
            PlaybackCommand::Play => proxy.call::<_, _, ()>("Play", &())?,
            PlaybackCommand::Pause => proxy.call::<_, _, ()>("Pause", &())?,
            PlaybackCommand::PlayPause => proxy.call::<_, _, ()>("PlayPause", &())?,
            PlaybackCommand::Next => proxy.call::<_, _, ()>("Next", &())?,
            PlaybackCommand::Previous => proxy.call::<_, _, ()>("Previous", &())?,
            PlaybackCommand::Repeat => {
                let current: String = proxy.get_property("LoopStatus")?;
                proxy.set_property(
                    "LoopStatus",
                    if current == "Track" { "None" } else { "Track" },
                )?;
            }
            PlaybackCommand::Shuffle => {
                let current: bool = proxy.get_property("Shuffle")?;
                proxy.set_property("Shuffle", !current)?;
            }
            PlaybackCommand::Seek(offset) => {
                proxy.call::<_, _, ()>("Seek", &(microseconds(offset)?,))?;
            }
            PlaybackCommand::SetPosition(position) => {
                ensure!(position.is_finite(), "position must be finite");
                let mut position = position.max(0.0);
                if player.snapshot.track.duration > 0.0 {
                    position = position.min(player.snapshot.track.duration);
                }
                let target = microseconds(position)?;
                let track_id = ObjectPath::try_from(player.snapshot.track.id.as_str())
                    .ok()
                    .filter(|path| {
                        path.as_str() != "/"
                            && path.as_str() != "/org/mpris/MediaPlayer2/TrackList/NoTrack"
                    });
                if let Some(track_id) = track_id {
                    match proxy.call::<_, _, ()>("SetPosition", &(track_id, target)) {
                        Ok(()) => return Ok(()),
                        Err(error) if unsupported(&error) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                // Read Position immediately: a cached/extrapolated position can
                // be stale after a seek, pause, or track transition.
                let current: i64 = proxy.get_property("Position")?;
                proxy.call::<_, _, ()>("Seek", &(target.saturating_sub(current),))?;
            }
            PlaybackCommand::Volume(volume) => {
                ensure!(volume.is_finite(), "volume must be finite");
                // MPRIS permits amplification above 1.0.
                proxy.set_property("Volume", volume.max(0.0))?;
            }
            PlaybackCommand::Open(uri) => {
                ensure!(!uri.trim().is_empty(), "URI must not be empty");
                if let Some(schemes) = player.root.get("SupportedUriSchemes") {
                    let schemes = Vec::<String>::try_from(schemes.try_clone()?)?;
                    let scheme = uri.split_once(':').map(|(scheme, _)| scheme);
                    ensure!(
                        schemes
                            .iter()
                            .any(|supported| Some(supported.as_str()) == scheme),
                        "MPRIS source does not support this URI scheme"
                    );
                }
                proxy.call::<_, _, ()>("OpenUri", &(uri,))?;
            }
        }
        Ok(())
    }
}

impl PlaybackBackend for MprisBackend {
    fn refresh(&mut self) -> Result<Vec<PlaybackSnapshot>> {
        self.receive_events();
        if self.disconnected {
            self.dirty = false;
            return Ok(Vec::new());
        }
        if self.discover {
            self.discover_players()?;
            self.discover = false;
        }
        let connection = &self.connection;
        self.players.retain(|_, player| {
            let loaded = (|| -> Result<()> {
                let mut position = player.snapshot.position_now();
                let reload = !player.invalid_root.is_empty() || !player.invalid_player.is_empty();
                let position_invalidated = player.invalid_player.contains("Position");
                reload_invalidated(
                    connection,
                    &player.owner,
                    ROOT,
                    &mut player.root,
                    &mut player.invalid_root,
                )?;
                reload_invalidated(
                    connection,
                    &player.owner,
                    PLAYER,
                    &mut player.properties,
                    &mut player.invalid_player,
                )?;
                if position_invalidated {
                    position = integer(&player.properties, "Position")
                        .map(seconds)
                        .unwrap_or(position);
                }
                let resync = player.resync_position
                    || player.position_synced_at.elapsed() >= POSITION_INTERVAL;
                if resync {
                    // Some otherwise usable players omit Position. Keep the
                    // extrapolated value when that optional read fails.
                    if let Ok(value) =
                        proxy(connection, &player.owner, PLAYER)?.get_property::<i64>("Position")
                    {
                        position = seconds(value);
                    }
                    player.resync_position = false;
                    player.position_synced_at = Instant::now();
                }
                // Only rebuild on actual I/O. Cached refreshes keep updated_at
                // stable and position_now() supplies smooth local extrapolation.
                if reload || resync {
                    player.snapshot = snapshot(
                        &player.snapshot.source_id,
                        &player.root,
                        &player.properties,
                        position,
                    );
                }
                Ok(())
            })();
            loaded.is_ok()
        });
        self.dirty = false;
        Ok(self.players.values().map(|p| p.snapshot.clone()).collect())
    }

    fn command(&mut self, source_id: &str, command: PlaybackCommand) -> Result<()> {
        // Resolve pending owner/capability changes before dispatching.
        self.refresh()?;
        let result = self.execute(source_id, command);
        if let Some(player) = self.players.get_mut(source_id) {
            player.resync_position = true;
        }
        // Commands are followed by a refresh even for players omitting signals.
        self.dirty = true;
        result
    }

    fn upcoming(&mut self, source_id: &str, limit: usize) -> Result<Vec<Track>> {
        self.receive_events();
        let limit = limit.min(3);
        if limit == 0 || self.disconnected {
            return Ok(vec![]);
        }
        let player = self
            .players
            .get(source_id)
            .context("MPRIS source disconnected")?;
        // HasTrackList is required by MPRIS. Browsers without it provide no
        // discoverable queue; never infer one from their current URL/title.
        let root = proxy(&self.connection, &player.owner, ROOT)?;
        match root.get_property::<bool>("HasTrackList") {
            Ok(true) => {}
            Ok(false) => return Ok(vec![]),
            Err(error) if missing_track_list(&error) => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        }
        let track_list = proxy(&self.connection, &player.owner, TRACK_LIST)?;
        let ids: Vec<zbus::zvariant::OwnedObjectPath> = match track_list.get_property("Tracks") {
            Ok(ids) => ids,
            Err(error) if missing_track_list(&error) => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        if boolean(&player.properties, "Shuffle") {
            return Ok(vec![]);
        }
        let current = &player.snapshot.track.id;
        if current.is_empty() || current == "/org/mpris/MediaPlayer2/TrackList/NoTrack" {
            return Ok(vec![]);
        }
        let Some(position) = ids.iter().position(|id| id.as_str() == current) else {
            return Ok(vec![]);
        };
        let next: Vec<_> = ids.into_iter().skip(position + 1).take(limit).collect();
        if next.is_empty() {
            return Ok(vec![]);
        }
        // Only the ID list is unbounded; request metadata for at most three.
        let metadata: Vec<Properties> = match track_list.call("GetTracksMetadata", &(&next,)) {
            Ok(metadata) => metadata,
            Err(error) if missing_track_list(&error) => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        // Match IDs rather than trusting reply ordering or stale queue entries.
        Ok(next
            .iter()
            .filter_map(|id| {
                metadata
                    .iter()
                    .map(metadata_track)
                    .find(|track| track.id == id.as_str())
            })
            .collect())
    }

    fn changed(&mut self) -> bool {
        self.receive_events();
        self.dirty
    }
}

impl Drop for MprisBackend {
    fn drop(&mut self) {
        let _ = self.timer_stop.send(());
        // Closing wakes the blocking iterator even when no player emits signals.
        let _ = self.connection.clone().close();
        if let Some(worker) = self.signal_worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.timer_worker.take() {
            let _ = worker.join();
        }
    }
}

fn proxy<'a>(connection: &'a Connection, owner: &'a str, interface: &'a str) -> Result<Proxy<'a>> {
    Ok(zbus::blocking::proxy::Builder::new(connection)
        .destination(owner)?
        .path(PATH)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()?)
}

fn get_all(connection: &Connection, owner: &str, interface: &str) -> Result<Properties> {
    Ok(proxy(connection, owner, PROPERTIES)?.call("GetAll", &(interface,))?)
}

fn reload_invalidated(
    connection: &Connection,
    owner: &str,
    interface: &str,
    properties: &mut Properties,
    invalidated: &mut BTreeSet<String>,
) -> Result<()> {
    if invalidated.is_empty() {
        return Ok(());
    }
    let proxy = proxy(connection, owner, PROPERTIES)?;
    for key in std::mem::take(invalidated) {
        match proxy.call::<_, _, OwnedValue>("Get", &(interface, key.as_str())) {
            Ok(value) => {
                properties.insert(key, value);
            }
            Err(zbus::Error::MethodError(name, _, _))
                if matches!(
                    name.as_str(),
                    "org.freedesktop.DBus.Error.UnknownProperty"
                        | "org.freedesktop.DBus.Error.NotSupported"
                ) =>
            {
                properties.remove(&key);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn signal_event(message: &Message) -> Option<Event> {
    let header = message.header();
    if header.message_type() != Type::Signal {
        return None;
    }
    let interface = header.interface()?.as_str();
    let member = header.member()?.as_str();
    let path = header.path()?.as_str();
    if interface == "org.freedesktop.DBus"
        && member == "NameOwnerChanged"
        && path == "/org/freedesktop/DBus"
        && header.sender()?.as_str() == "org.freedesktop.DBus"
    {
        let (name, _, _): (String, String, String) = message.body().deserialize().ok()?;
        return name.starts_with(PREFIX).then_some(Event::Owners(name));
    }
    if path != PATH {
        return None;
    }
    let sender = header.sender()?.to_string();
    if interface == PROPERTIES && member == "PropertiesChanged" {
        let (interface, changed, invalidated): (String, Properties, Vec<String>) =
            message.body().deserialize().ok()?;
        if interface == ROOT || interface == PLAYER {
            return Some(Event::Properties(sender, interface, changed, invalidated));
        }
    } else if interface == PLAYER && member == "Seeked" {
        let (position,): (i64,) = message.body().deserialize().ok()?;
        return Some(Event::Seeked(sender, position));
    }
    None
}

fn snapshot(
    source_id: &str,
    root: &Properties,
    properties: &Properties,
    position: f64,
) -> PlaybackSnapshot {
    let metadata = properties
        .get("Metadata")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Properties::try_from(v).ok())
        .unwrap_or_default();
    let track = metadata_track(&metadata);
    let duration = track.duration;
    let control = boolean(properties, "CanControl");
    PlaybackSnapshot {
        source_id: source_id.into(),
        source_name: string(root, "Identity").unwrap_or_else(|| {
            source_id
                .strip_prefix(PREFIX)
                .unwrap_or(source_id)
                .to_owned()
        }),
        track,
        status: match string(properties, "PlaybackStatus").as_deref() {
            Some("Playing") => PlaybackStatus::Playing,
            Some("Paused") => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        },
        position: if duration > 0.0 {
            position.clamp(0.0, duration)
        } else {
            position.max(0.0)
        },
        rate: number(properties, "Rate")
            .filter(|rate| *rate != 0.0)
            .unwrap_or(1.0),
        volume: number(properties, "Volume").unwrap_or(1.0).max(0.0),
        capabilities: Capabilities {
            play: control && boolean(properties, "CanPlay"),
            pause: control && boolean(properties, "CanPause"),
            seek: control && boolean(properties, "CanSeek"),
            next: control && boolean(properties, "CanGoNext"),
            previous: control && boolean(properties, "CanGoPrevious"),
            volume: control && number(properties, "Volume").is_some(),
        },
        updated_at: Instant::now(),
    }
}

fn metadata_track(metadata: &Properties) -> Track {
    let artist = metadata
        .get("xesam:artist")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<String>::try_from(v).ok())
        .map(|artists| artists.join(", "))
        .unwrap_or_else(|| string(metadata, "xesam:artist").unwrap_or_default());
    let duration = integer(metadata, "mpris:length")
        .map(seconds)
        .unwrap_or(0.0);
    Track {
        id: metadata
            .get("mpris:trackid")
            .and_then(|v| <&ObjectPath<'_>>::try_from(v).ok())
            .map(|path| path.as_str().to_owned())
            .unwrap_or_default(),
        title: string(metadata, "xesam:title").unwrap_or_default(),
        artist,
        album: string(metadata, "xesam:album").unwrap_or_default(),
        artwork_url: string(metadata, "mpris:artUrl"),
        url: string(metadata, "xesam:url"),
        duration,
    }
}

fn string(properties: &Properties, key: &str) -> Option<String> {
    properties
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .map(str::to_owned)
}

fn boolean(properties: &Properties, key: &str) -> bool {
    properties
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn integer(properties: &Properties, key: &str) -> Option<i64> {
    properties.get(key).and_then(|v| i64::try_from(v).ok())
}

fn number(properties: &Properties, key: &str) -> Option<f64> {
    properties
        .get(key)
        .and_then(|v| f64::try_from(v).ok())
        .filter(|v| v.is_finite())
}

fn seconds(microseconds: i64) -> f64 {
    (microseconds as f64 / 1_000_000.0).max(0.0)
}

fn microseconds(seconds: f64) -> Result<i64> {
    ensure!(seconds.is_finite(), "time must be finite");
    let value = seconds * 1_000_000.0;
    if value < i64::MIN as f64 || value >= i64::MAX as f64 {
        bail!("time is outside the MPRIS microsecond range");
    }
    Ok(value.round() as i64)
}

fn unsupported(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::MethodError(name, _, _) if matches!(
        name.as_str(),
        "org.freedesktop.DBus.Error.UnknownMethod"
            | "org.freedesktop.DBus.Error.NotSupported"
            | "org.mpris.MediaPlayer2.Player.Error.NotSupported"
    ))
}

fn missing_track_list(error: &zbus::Error) -> bool {
    unsupported(error)
        || matches!(error, zbus::Error::MethodError(name, _, _) if matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.UnknownProperty"
        ))
}
