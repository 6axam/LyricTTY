//! Blocking lyrics I/O. Call from an assets worker, never the terminal thread.
//!
//! LRCLIB's current API returns Lyricsfile YAML in `lyricsfile`; word timestamps
//! live there, not in the legacy `syncedLyrics` field. Schema/reference:
//! https://lrclib.net/docs.md and https://github.com/tranxuanthang/lyricsfile
use crate::{
    lyrics::{parse_lrc, LyricLine, LyricWord},
    playback::Track,
};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::{blocking::Client, header, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
const MAX_LINES: usize = 20_000;
const MAX_WORDS: usize = 100_000;
const USER_AGENT: &str = concat!(
    "LyricTTY/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/6axam/LyricTTY)"
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LyricsQuality {
    Word,
    Line,
    Plain,
    #[default]
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LyricsDocument {
    pub lines: Vec<LyricLine>,
    pub plain: String,
    pub quality: LyricsQuality,
    pub instrumental: bool,
    pub source: String,
    pub cached: bool,
}

impl LyricsDocument {
    pub fn quality_label(&self) -> &'static str {
        if self.instrumental {
            return "Instrumental";
        }
        match self.quality {
            LyricsQuality::Word => "Word synced",
            LyricsQuality::Line => "Line synced",
            LyricsQuality::Plain => "Plain lyrics",
            LyricsQuality::None => "No lyrics",
        }
    }

    fn from_parts(lines: Vec<LyricLine>, plain: String, instrumental: bool, source: &str) -> Self {
        let quality = if lines
            .iter()
            .any(|l| l.words.iter().any(|w| w.end > w.start))
        {
            LyricsQuality::Word
        } else if !lines.is_empty() {
            LyricsQuality::Line
        } else if !plain.trim().is_empty() {
            LyricsQuality::Plain
        } else {
            LyricsQuality::None
        };
        Self {
            lines,
            plain,
            quality,
            instrumental,
            source: source.into(),
            cached: false,
        }
    }
}

/// `None` means a definitive miss; errors (including rate limiting) are not
/// misses and must never be written into the negative cache.
pub trait LyricsProvider: Send + Sync {
    fn name(&self) -> &str;
    fn cacheable(&self) -> bool {
        true
    }
    fn cache_namespace(&self) -> String {
        self.name().into()
    }
    fn lookup(&self, track: &Track) -> Result<Option<LyricsDocument>>;
}

#[derive(Clone, Debug)]
pub struct LocalLyrics {
    path: PathBuf,
}

impl LocalLyrics {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn load(&self) -> Result<LyricsDocument> {
        load_local(&self.path)
    }
}

impl LyricsProvider for LocalLyrics {
    fn name(&self) -> &str {
        "Local"
    }
    fn cacheable(&self) -> bool {
        false
    }
    fn lookup(&self, _track: &Track) -> Result<Option<LyricsDocument>> {
        self.load().map(Some)
    }
}

/// Explicit CLI file: ordinary/enhanced .lrc, Lyricsfile JSON, or the official
/// .lyricsfile.yaml format. Local files are reread and are not remotely cached.
pub fn load_local(path: impl AsRef<Path>) -> Result<LyricsDocument> {
    let path = path.as_ref();
    let input = read_bounded(
        File::open(path).with_context(|| format!("Open lyrics {}", path.display()))?,
        MAX_DOCUMENT_BYTES,
    )?;
    let input = std::str::from_utf8(&input)
        .context("Lyrics file is not UTF-8")?
        .trim_start_matches('\u{feff}');
    let extension = path.extension().and_then(|x| x.to_str()).unwrap_or("");
    let source = format!(
        "Local: {}",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let mut document =
        if extension.eq_ignore_ascii_case("json") || input.trim_start().starts_with('{') {
            parse_lyricsfile_json(input)?
        } else if extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml") {
            parse_lyricsfile_yaml(input)?
        } else {
            let lines = parse_lrc(input);
            if lines.is_empty() && input.lines().any(|line| line.trim_start().starts_with('[')) {
                bail!("Lyrics file contains no usable LRC timestamps");
            }
            let plain = if lines.is_empty() {
                input.to_owned()
            } else {
                line_text(&lines)
            };
            LyricsDocument::from_parts(lines, plain, false, "Local")
        };
    document.source = source;
    Ok(document)
}

#[derive(Deserialize)]
struct Lyricsfile {
    version: String,
    metadata: LyricsMetadata,
    #[serde(default)]
    lines: Vec<FileLine>,
    #[serde(default)]
    plain: String,
}

#[derive(Deserialize)]
struct LyricsMetadata {
    // Required by the specification, even though the renderer doesn't use them.
    title: String,
    artist: String,
    duration_ms: Option<u64>,
    #[serde(default)]
    instrumental: bool,
    // offset_ms is intentionally not applied: its semantics are explicitly
    // unresolved in Lyricsfile 1.0. Unlike LRC there is no standard offset rule.
}

#[derive(Deserialize)]
struct FileLine {
    text: String,
    start_ms: u64,
    end_ms: Option<u64>,
    #[serde(default)]
    words: Vec<FileWord>,
}

#[derive(Deserialize)]
struct FileWord {
    text: String,
    start_ms: u64,
    end_ms: Option<u64>,
}

pub fn parse_lyricsfile_json(input: &str) -> Result<LyricsDocument> {
    check_size(input)?;
    lyricsfile_document(serde_json::from_str(input).context("Invalid Lyricsfile JSON")?)
}

pub fn parse_lyricsfile_yaml(input: &str) -> Result<LyricsDocument> {
    check_size(input)?;
    // Typed deserialization rejects duplicate schema fields, invalid timestamp
    // types, and recursion/alias abuse using serde_yaml's parser limits.
    lyricsfile_document(serde_yaml::from_str(input).context("Invalid Lyricsfile YAML")?)
}

fn check_size(input: &str) -> Result<()> {
    if input.len() > MAX_DOCUMENT_BYTES {
        bail!("Lyrics document exceeds size limit");
    }
    Ok(())
}

fn milliseconds(ms: u64) -> f32 {
    (ms as f64 / 1000.0) as f32
}

fn lyricsfile_document(mut file: Lyricsfile) -> Result<LyricsDocument> {
    if file.version != "1.0" {
        bail!("Unsupported Lyricsfile version: {}", file.version);
    }
    let _metadata = (&file.metadata.title, &file.metadata.artist);
    if file.lines.len() > MAX_LINES
        || file.lines.iter().map(|l| l.words.len()).sum::<usize>() > MAX_WORDS
    {
        bail!("Too many lyric lines or words");
    }
    if file.metadata.instrumental {
        if !file.lines.is_empty() || !file.plain.trim().is_empty() {
            bail!("Instrumental Lyricsfile contains lyrics");
        }
        return Ok(LyricsDocument::from_parts(
            Vec::new(),
            String::new(),
            true,
            "Lyricsfile",
        ));
    }
    file.lines.sort_by_key(|line| line.start_ms);
    let mut lines = Vec::with_capacity(file.lines.len());
    let mut group_end = 0;
    for (index, line) in file.lines.iter().enumerate() {
        if index >= group_end {
            group_end = index + 1;
            while group_end < file.lines.len() && file.lines[group_end].start_ms == line.start_ms {
                group_end += 1;
            }
        }
        if line.end_ms.is_some_and(|end| end < line.start_ms) {
            bail!("Lyricsfile line ends before it starts");
        }
        let start = milliseconds(line.start_ms);
        let inferred_end = file
            .lines
            .get(group_end)
            .map(|l| l.start_ms)
            .or(file.metadata.duration_ms.filter(|d| *d > line.start_ms));
        // Only display ends are inferred; real explicit overlapping ends remain.
        let end = line
            .end_ms
            .or(inferred_end)
            .map(milliseconds)
            .unwrap_or(start + 4.0)
            .max(start);
        let mut words = Vec::with_capacity(line.words.len());
        for (i, word) in line.words.iter().enumerate() {
            if word.end_ms.is_some_and(|end| end < word.start_ms) {
                bail!("Lyricsfile word ends before it starts");
            }
            let next = line
                .words
                .get(i + 1)
                .map(|w| w.start_ms)
                .filter(|next| *next >= word.start_ms);
            // A next real word cue or an explicit line end bounds a word. The
            // final unbounded word is zero-length, rather than fake karaoke.
            let word_end = word
                .end_ms
                .or(next)
                .or(line.end_ms.filter(|e| *e >= word.start_ms))
                .unwrap_or(word.start_ms);
            words.push(LyricWord {
                text: word.text.clone(),
                start: milliseconds(word.start_ms),
                end: milliseconds(word_end),
            });
        }
        if words.iter().map(|w| w.text.as_str()).collect::<String>() != line.text {
            // Preserve readable text when a contributor supplied broken segments.
            words.clear();
        }
        if !line.text.is_empty() {
            lines.push(LyricLine {
                text: line.text.clone(),
                start,
                end,
                words,
            });
        }
    }
    let plain = if file.plain.is_empty() {
        line_text(&lines)
    } else {
        file.plain
    };
    Ok(LyricsDocument::from_parts(
        lines,
        plain,
        false,
        "Lyricsfile",
    ))
}

fn line_text(lines: &[LyricLine]) -> String {
    lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bounded, atomic disk cache. Corrupt/expired entries and I/O failures are
/// treated as misses. Positive TTL is seven days, negative TTL six hours.
#[derive(Clone, Debug)]
pub struct LyricsCache {
    root: PathBuf,
    positive_ttl: Duration,
    negative_ttl: Duration,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    version: u32,
    key: String,
    created_at: u64,
    document: LyricsDocument,
}

impl LyricsCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            positive_ttl: Duration::from_secs(7 * 86400),
            negative_ttl: Duration::from_secs(6 * 3600),
        }
    }
    pub fn default_location() -> Option<Self> {
        dirs::cache_dir().map(|root| Self::new(root.join("lyrictty").join("lyrics-v1")))
    }
    pub fn with_ttl(mut self, positive: Duration, negative: Duration) -> Self {
        self.positive_ttl = positive;
        self.negative_ttl = negative;
        self
    }
    fn get(&self, key: &str) -> Option<LyricsDocument> {
        let path = self.root.join(format!("{key}.json"));
        let entry = (|| -> Result<CacheEntry> {
            let bytes = read_bounded(File::open(&path)?, MAX_CACHE_BYTES)?;
            Ok(serde_json::from_slice(&bytes)?)
        })();
        if let Ok(mut entry) = entry {
            let ttl =
                if entry.document.quality == LyricsQuality::None && !entry.document.instrumental {
                    self.negative_ttl
                } else {
                    self.positive_ttl
                };
            if entry.version == 1
                && entry.key == key
                && unix_now()
                    .checked_sub(entry.created_at)
                    .is_some_and(|age| age < ttl.as_secs())
                && valid_document(&entry.document)
            {
                entry.document.cached = true;
                return Some(entry.document);
            }
        }
        let _ = fs::remove_file(path);
        None
    }
    fn put(&self, key: &str, document: &LyricsDocument) -> Result<()> {
        if !valid_document(document) {
            bail!("Invalid lyric timings cannot be cached");
        }
        fs::create_dir_all(&self.root)?;
        let bytes = serde_json::to_vec(&CacheEntry {
            version: 1,
            key: key.into(),
            created_at: unix_now(),
            document: document.clone(),
        })?;
        if bytes.len() > MAX_CACHE_BYTES {
            bail!("Lyrics cache entry exceeds size limit");
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.root.join(format!("{key}.json")))
            .map_err(|e| e.error)?;
        self.prune();
        Ok(())
    }
    fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        let mut files: Vec<_> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let stem = name.to_str()?.strip_suffix(".json")?;
                if stem.len() != 64 || !stem.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                if !metadata.is_file() {
                    return None;
                }
                Some((
                    metadata.modified().unwrap_or(UNIX_EPOCH),
                    metadata.len(),
                    entry.path(),
                ))
            })
            .collect();
        files.sort_by_key(|entry| entry.0);
        let mut bytes: u64 = files.iter().map(|entry| entry.1).sum();
        let mut count = files.len();
        for (_, size, path) in files {
            if count <= 512 && bytes <= 128 * 1024 * 1024 {
                break;
            }
            if fs::remove_file(path).is_ok() {
                count -= 1;
                bytes = bytes.saturating_sub(size);
            }
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn valid_document(doc: &LyricsDocument) -> bool {
    doc.lines.len() <= MAX_LINES
        && doc.lines.iter().map(|l| l.words.len()).sum::<usize>() <= MAX_WORDS
        && doc.lines.iter().all(|line| {
            line.start.is_finite()
                && line.end.is_finite()
                && line.start >= 0.0
                && line.end >= line.start
                && line.words.iter().all(|word| {
                    word.start.is_finite()
                        && word.end.is_finite()
                        && word.start >= 0.0
                        && word.end >= word.start
                })
        })
}

fn cache_key(track: &Track, providers: &[Box<dyn LyricsProvider>]) -> String {
    let normalize = |text: &str| {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let duration = if track.duration.is_finite() && track.duration > 0.0 {
        Some((track.duration * 1000.0).round().min(u64::MAX as f64) as u64)
    } else {
        None
    };
    // JSON framing avoids concatenation collisions and filenames contain no
    // metadata, slashes, URLs, or track IDs. Provider order is part of identity.
    let framed = serde_json::to_vec(&(
        1,
        providers
            .iter()
            .map(|p| p.cache_namespace())
            .collect::<Vec<_>>(),
        normalize(&track.title),
        normalize(&track.artist),
        normalize(&track.album),
        duration,
    ))
    .unwrap_or_default();
    format!("{:x}", Sha256::digest(framed))
}

pub struct LyricsService {
    providers: Vec<Box<dyn LyricsProvider>>,
    cache: Option<LyricsCache>,
    local: Option<LocalLyrics>,
}

impl LyricsService {
    pub fn new(cache: Option<LyricsCache>) -> Result<Self> {
        Ok(Self::with_providers(
            vec![Box::new(LrclibProvider::new()?)],
            cache,
        ))
    }
    pub fn with_providers(
        providers: Vec<Box<dyn LyricsProvider>>,
        cache: Option<LyricsCache>,
    ) -> Self {
        Self {
            providers,
            cache,
            local: None,
        }
    }
    pub fn with_local_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.local = Some(LocalLyrics::new(path));
        self
    }
    pub fn lookup(&self, track: &Track) -> Result<LyricsDocument> {
        if let Some(local) = &self.local {
            return local.load();
        }
        let key = cache_key(track, &self.providers);
        let cache = self
            .cache
            .as_ref()
            .filter(|_| self.providers.iter().all(|p| p.cacheable()));
        if let Some(document) = cache.and_then(|cache| cache.get(&key)) {
            return Ok(document);
        }
        let mut errors = Vec::new();
        for provider in &self.providers {
            match provider.lookup(track) {
                Ok(Some(mut document)) => {
                    if !valid_document(&document) {
                        errors.push(format!("{}: invalid lyric timings", provider.name()));
                        continue;
                    }
                    if document.source.is_empty() {
                        document.source = provider.name().into();
                    }
                    document.cached = false;
                    if let Some(cache) = cache {
                        let _ = cache.put(&key, &document);
                    }
                    return Ok(document);
                }
                Ok(None) => (),
                Err(error) => errors.push(format!("{}: {error:#}", provider.name())),
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        let document = LyricsDocument {
            source: self
                .providers
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", "),
            ..Default::default()
        };
        if let Some(cache) = cache {
            let _ = cache.put(&key, &document);
        }
        Ok(document)
    }
}

/// Official LRCLIB v1 signature lookup, then a conservative search fallback.
/// Requests are serialized, spaced by 250ms, and 429/503 cooldowns persist across
/// lookups without blocking the worker for a long Retry-After sleep.
pub struct LrclibProvider {
    client: Client,
    base_url: String,
    gate: Mutex<RequestGate>,
}

struct RequestGate {
    next_request: Instant,
    retry_after: Option<(Instant, Duration)>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibRecord {
    track_name: String,
    artist_name: String,
    #[serde(default)]
    album_name: String,
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
    lyricsfile: Option<serde_json::Value>,
}

impl LrclibProvider {
    pub fn new() -> Result<Self> {
        Self::with_base_url("https://lrclib.net")
    }

    /// Alternate origin is useful for self-hosted LRCLIB and offline HTTP tests.
    pub fn with_base_url(base_url: &str) -> Result<Self> {
        let url = reqwest::Url::parse(base_url)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("Invalid LRCLIB base URL");
        }
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::limited(2))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').into(),
            gate: Mutex::new(RequestGate {
                next_request: Instant::now(),
                retry_after: None,
            }),
        })
    }

    fn request(&self, path: &str, query: &[(&str, String)]) -> Result<Option<serde_json::Value>> {
        let mut gate = self
            .gate
            .lock()
            .map_err(|_| anyhow!("LRCLIB request lock poisoned"))?;
        if let Some((since, delay)) = gate
            .retry_after
            .filter(|(since, delay)| since.elapsed() < *delay)
        {
            bail!(
                "LRCLIB cooldown: retry in {} seconds",
                delay
                    .saturating_sub(since.elapsed())
                    .as_secs()
                    .saturating_add(1)
            );
        }
        if let Some(delay) = gate.next_request.checked_duration_since(Instant::now()) {
            std::thread::sleep(delay);
        }
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .header(header::ACCEPT, "application/json")
            .query(query)
            .send();
        gate.next_request = Instant::now() + Duration::from_millis(250);
        let response = response.context("LRCLIB HTTP request failed")?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            let seconds = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|x| x.to_str().ok())
                .and_then(|x| x.parse::<u64>().ok())
                .unwrap_or(60);
            // LRCLIB documents integer seconds. Store a relative duration so
            // even a very large Retry-After cannot overflow an Instant.
            let delay = Duration::from_secs(seconds)
                .saturating_add(Duration::from_millis(1 + (unix_now() % 251)));
            gate.retry_after = Some((Instant::now(), delay));
            bail!("LRCLIB HTTP {status}; Retry-After {seconds} seconds");
        }
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            bail!("LRCLIB HTTP {status}");
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|x| x.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if content_type != "application/json" && !content_type.ends_with("+json") {
            bail!("LRCLIB returned non-JSON content");
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_DOCUMENT_BYTES as u64)
        {
            bail!("LRCLIB response exceeds size limit");
        }
        let body = read_bounded(response, MAX_DOCUMENT_BYTES)?;
        Ok(Some(
            serde_json::from_slice(&body).context("Malformed LRCLIB JSON response")?,
        ))
    }
}

impl LyricsProvider for LrclibProvider {
    fn name(&self) -> &str {
        "LRCLIB"
    }
    fn cache_namespace(&self) -> String {
        format!("LRCLIB:{}", self.base_url)
    }
    fn lookup(&self, track: &Track) -> Result<Option<LyricsDocument>> {
        if track.title.trim().is_empty() || track.artist.trim().is_empty() {
            return Ok(None);
        }
        let mut query = vec![
            ("track_name", track.title.trim().into()),
            ("artist_name", track.artist.trim().into()),
        ];
        if !track.album.trim().is_empty() {
            query.push(("album_name", track.album.trim().into()));
        }
        if track.duration.is_finite() && (1.0..=3600.0).contains(&track.duration) {
            query.push(("duration", track.duration.to_string()));
        }
        let mut provisional = None;
        if let Some(value) = self.request("/api/get", &query)? {
            let record: LrclibRecord =
                serde_json::from_value(value).context("Invalid LRCLIB record")?;
            if let Some(document) = record_document(record)? {
                // Community records occasionally contain a single test cue for
                // an otherwise complete song. Keep it as fallback while checking
                // matching releases, rather than declaring lookup complete.
                if document.quality == LyricsQuality::Line
                    && document.lines.len() < 3
                    && track.duration > 60.0
                {
                    provisional = Some(document);
                } else {
                    return Ok(Some(document));
                }
            }
        }
        query.retain(|(name, _)| *name != "duration" && *name != "album_name");
        let value = match self.request("/api/search", &query) {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(provisional),
            Err(_) if provisional.is_some() => return Ok(provisional),
            Err(e) => return Err(e),
        };
        let records: Vec<LrclibRecord> =
            serde_json::from_value(value).context("Invalid LRCLIB search response")?;
        if records.len() > 20 {
            bail!("LRCLIB returned too many search results");
        }
        let mut matching: Vec<_> = records
            .into_iter()
            .filter(|record| matches_track(record, track))
            .collect();
        matching.sort_by(|a, b| {
            let album = |r: &LrclibRecord| !same_text(&r.album_name, &track.album);
            album(a).cmp(&album(b)).then_with(|| {
                (a.duration - track.duration)
                    .abs()
                    .total_cmp(&(b.duration - track.duration).abs())
            })
        });
        for record in matching {
            if let Some(document) = record_document(record)? {
                return Ok(Some(document));
            }
        }
        Ok(provisional)
    }
}

fn same_text(a: &str, b: &str) -> bool {
    a.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        == b.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
}

fn matches_track(record: &LrclibRecord, track: &Track) -> bool {
    same_text(&record.track_name, &track.title)
        && same_text(&record.artist_name, &track.artist)
        && (track.duration <= 0.0
            || !track.duration.is_finite()
            || (record.duration.is_finite() && (record.duration - track.duration).abs() <= 2.0))
}

fn record_document(record: LrclibRecord) -> Result<Option<LyricsDocument>> {
    if record.instrumental {
        return Ok(Some(LyricsDocument::from_parts(
            Vec::new(),
            String::new(),
            true,
            "LRCLIB",
        )));
    }
    let mut lyricsfile_plain = None;
    let mut lyricsfile_error = None;
    if let Some(value) = record.lyricsfile {
        let parsed = match value {
            serde_json::Value::String(raw) => {
                if raw.trim_start().starts_with('{') {
                    parse_lyricsfile_json(&raw)
                } else {
                    parse_lyricsfile_yaml(&raw)
                }
            }
            serde_json::Value::Object(_) => serde_json::from_value(value)
                .map_err(Into::into)
                .and_then(lyricsfile_document),
            _ => Err(anyhow!("Unsupported LRCLIB Lyricsfile value")),
        };
        match parsed {
            Ok(mut document) => {
                document.source = "LRCLIB".into();
                if matches!(document.quality, LyricsQuality::Word | LyricsQuality::Line)
                    || document.instrumental
                {
                    return Ok(Some(document));
                }
                if document.quality == LyricsQuality::Plain {
                    lyricsfile_plain = Some(document);
                }
            }
            Err(error) => lyricsfile_error = Some(error),
        }
    }
    let lines = record
        .synced_lyrics
        .as_deref()
        .map(parse_lrc)
        .unwrap_or_default();
    let plain = record
        .plain_lyrics
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| line_text(&lines));
    let document = LyricsDocument::from_parts(lines, plain, false, "LRCLIB");
    if !document.lines.is_empty() {
        return Ok(Some(document));
    }
    if let Some(document) = lyricsfile_plain {
        return Ok(Some(document));
    }
    if document.quality != LyricsQuality::None {
        return Ok(Some(document));
    }
    if let Some(error) = lyricsfile_error {
        return Err(error.context("LRCLIB Lyricsfile could not be read and has no usable fallback"));
    }
    Ok(None)
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("Lyrics data exceeds size limit ({limit} bytes)");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::{active_line, active_word};
    use std::{
        net::TcpListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    const WORDS_JSON: &str = include_str!("../tests/fixtures/lyrics.words.json");
    const WORDS_YAML: &str = include_str!("../tests/fixtures/lyrics.words.yaml");

    fn track() -> Track {
        Track {
            title: "Ранок".into(),
            artist: "Тестовий виконавець".into(),
            album: "Album".into(),
            duration: 12.0,
            ..Default::default()
        }
    }
    fn plain() -> LyricsDocument {
        LyricsDocument::from_parts(Vec::new(), "Тільки текст".into(), false, "fake")
    }

    #[test]
    fn lyricsfile_json_and_official_yaml_use_integer_milliseconds() {
        let doc = parse_lyricsfile_json(WORDS_JSON).unwrap();
        assert_eq!(doc, parse_lyricsfile_yaml(WORDS_YAML).unwrap());
        assert_eq!(doc.quality, LyricsQuality::Word);
        assert_eq!(doc.lines[0].start, 1.2);
        assert_eq!(doc.lines[0].words[0].end, 1.9);
        assert_eq!(doc.lines[0].words[0].text, "Привіт, ");
        assert_eq!(active_line(&doc.lines, 2.8), None);
        assert_eq!(active_line(&doc.lines, 4.0), Some(1));
        assert_eq!(active_word(&doc.lines[0], 1.9), Some(1));
        assert_eq!(active_word(&doc.lines[0], 2.8), None);
        assert_eq!(doc.plain, "Привіт, світе 🌍\n\nНова пісня");
    }

    #[test]
    fn lyricsfile_rejects_bad_versions_types_duplicates_and_reversed_times() {
        for input in [
            WORDS_JSON.replace("\"1.0\"", "\"2.0\""),
            WORDS_JSON.replace("1200", "-1"),
            WORDS_JSON.replace("1200", "1.2"),
            WORDS_JSON.replace("1900", "1199"),
            WORDS_JSON.replace(
                "\"version\": \"1.0\",",
                "\"version\": \"1.0\", \"version\": \"1.0\",",
            ),
        ] {
            assert!(parse_lyricsfile_json(&input).is_err(), "{input}");
        }
        assert!(parse_lyricsfile_yaml(
            &WORDS_YAML.replace("version: '1.0'", "version: '1.0'\nversion: '1.0'")
        )
        .is_err());
        assert!(parse_lyricsfile_yaml("---\nversion: '1.0'\n---\nversion: '1.0'").is_err());
        assert!(parse_lyricsfile_json("{}").is_err());
    }

    #[test]
    fn preserves_explicit_overlaps_and_does_not_invent_word_ends() {
        let doc = parse_lyricsfile_json(
            r#"{
            "version":"1.0", "metadata":{"title":"a","artist":"b"},
            "lines":[
                {"text":"a b", "start_ms":1000, "end_ms":5000, "words":[
                    {"text":"a ","start_ms":1000}, {"text":"b","start_ms":2000}]},
                {"text":"c", "start_ms":3000, "end_ms":4000},
                {"text":"last", "start_ms":6000, "words":[{"text":"last","start_ms":6000}]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(doc.lines[0].end, 5.0);
        assert_eq!(doc.lines[0].words[0].end, 2.0);
        assert_eq!(doc.lines[0].words[1].end, 5.0);
        assert_eq!(active_line(&doc.lines, 4.5), Some(0));
        assert_eq!(doc.lines[2].words[0].start, doc.lines[2].words[0].end);
    }

    #[test]
    fn malformed_segments_fall_back_to_full_line_text() {
        let doc = parse_lyricsfile_json(
            &WORDS_JSON.replace("\"text\": \"Привіт, \",", "\"text\": \"Wrong\","),
        )
        .unwrap();
        assert_eq!(doc.quality, LyricsQuality::Line);
        assert!(doc.lines[0].words.is_empty());
        assert_eq!(doc.lines[0].text, "Привіт, світе 🌍");
    }

    #[test]
    fn supports_plain_instrumental_and_unknown_absence_separately() {
        let doc = parse_lyricsfile_json(
            r#"{"version":"1.0","metadata":{"title":"a","artist":"b"},"plain":"Без часу"}"#,
        )
        .unwrap();
        assert_eq!(doc.quality, LyricsQuality::Plain);
        assert!(doc.lines.is_empty());
        let doc = parse_lyricsfile_yaml(
            "version: '1.0'\nmetadata:\n  title: a\n  artist: b\n  instrumental: true\n",
        )
        .unwrap();
        assert!(doc.instrumental);
        assert_eq!(doc.quality_label(), "Instrumental");
        assert_eq!(LyricsDocument::default().quality_label(), "No lyrics");
        assert!(!LyricsDocument::default().instrumental);
    }

    #[test]
    fn current_api_has_both_name_and_track_name_and_yaml_word_sync() {
        let record: LrclibRecord = serde_json::from_value(serde_json::json!({
            "name":"Ранок", "trackName":"Ранок", "artistName":"Тестовий виконавець",
            "hasWordSync":true, "lyricsfile": WORDS_YAML,
            "syncedLyrics":"[00:01.20]Привіт, світе 🌍"
        }))
        .unwrap();
        let doc = record_document(record).unwrap().unwrap();
        assert_eq!(doc.quality, LyricsQuality::Word);
        assert_eq!(doc.source, "LRCLIB");
    }

    #[test]
    fn online_falls_back_from_broken_yaml_to_lrc_then_plain() {
        let mut record = LrclibRecord {
            lyricsfile: Some(serde_json::Value::String("version: [bad".into())),
            synced_lyrics: Some("[00:01]Тест\n[00:03]Другий".into()),
            plain_lyrics: Some("Тест\nДругий".into()),
            ..Default::default()
        };
        let doc = record_document(record.clone()).unwrap().unwrap();
        assert_eq!(doc.quality, LyricsQuality::Line);
        assert!(doc.lines.iter().all(|l| l.words.is_empty()));
        record.synced_lyrics = None;
        assert_eq!(
            record_document(record.clone()).unwrap().unwrap().quality,
            LyricsQuality::Plain
        );
        record.plain_lyrics = None;
        assert!(record_document(record).is_err());
    }

    struct FakeProvider {
        calls: Arc<AtomicUsize>,
        document: Option<LyricsDocument>,
        fail: bool,
    }
    impl LyricsProvider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        fn lookup(&self, _track: &Track) -> Result<Option<LyricsDocument>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                bail!("temporary network failure");
            }
            Ok(self.document.clone())
        }
    }
    fn fake(
        document: Option<LyricsDocument>,
        fail: bool,
    ) -> (Box<dyn LyricsProvider>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Box::new(FakeProvider {
                calls: calls.clone(),
                document,
                fail,
            }),
            calls,
        )
    }

    #[test]
    fn positive_and_negative_cache_avoid_provider_calls() {
        for document in [Some(plain()), None] {
            let dir = tempfile::tempdir().unwrap();
            let (provider, calls) = fake(document.clone(), false);
            let service =
                LyricsService::with_providers(vec![provider], Some(LyricsCache::new(dir.path())));
            assert!(!service.lookup(&track()).unwrap().cached);
            let second = service.lookup(&track()).unwrap();
            assert!(second.cached);
            assert_eq!(
                second.quality,
                if document.is_some() {
                    LyricsQuality::Plain
                } else {
                    LyricsQuality::None
                }
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn cache_keys_hash_metadata_and_include_album_duration_and_provider() {
        let (provider, _) = fake(None, false);
        let providers = vec![provider];
        let mut input = track();
        input.title = "../../Привіт".into();
        let original = cache_key(&input, &providers);
        assert_eq!(original.len(), 64);
        assert!(original.bytes().all(|c| c.is_ascii_hexdigit()));
        input.duration += 1.0;
        assert_ne!(original, cache_key(&input, &providers));
        input.duration -= 1.0;
        input.album.push('x');
        assert_ne!(original, cache_key(&input, &providers));
        assert_ne!(original, cache_key(&input, &[]));
    }

    #[test]
    fn corrupt_expired_future_and_mismatched_cache_entries_are_misses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path());
        let key = "a".repeat(64);
        let path = dir.path().join(format!("{key}.json"));
        fs::write(&path, "broken JSON").unwrap();
        assert!(cache.get(&key).is_none());
        for (created_at, version, stored_key) in [
            (0, 1, key.clone()),
            (u64::MAX, 1, key.clone()),
            (unix_now(), 99, key.clone()),
            (unix_now(), 1, "b".repeat(64)),
        ] {
            fs::write(
                &path,
                serde_json::to_vec(&CacheEntry {
                    version,
                    key: stored_key,
                    created_at,
                    document: plain(),
                })
                .unwrap(),
            )
            .unwrap();
            assert!(cache.get(&key).is_none());
        }
        cache.put(&key, &plain()).unwrap();
        assert!(cache
            .clone()
            .with_ttl(Duration::ZERO, Duration::ZERO)
            .get(&key)
            .is_none());
    }

    #[test]
    fn negative_ttl_is_shorter_than_positive_and_instrumental_is_positive() {
        let dir = tempfile::tempdir().unwrap();
        let cache =
            LyricsCache::new(dir.path()).with_ttl(Duration::from_secs(100), Duration::from_secs(5));
        let key = "b".repeat(64);
        for (document, expected_hit) in [
            (plain(), true),
            (LyricsDocument::default(), false),
            (
                LyricsDocument {
                    instrumental: true,
                    ..Default::default()
                },
                true,
            ),
        ] {
            let entry = CacheEntry {
                version: 1,
                key: key.clone(),
                created_at: unix_now() - 10,
                document,
            };
            fs::write(
                dir.path().join(format!("{key}.json")),
                serde_json::to_vec(&entry).unwrap(),
            )
            .unwrap();
            assert_eq!(cache.get(&key).is_some(), expected_hit);
        }
    }

    #[test]
    fn errors_are_not_negative_cached_and_fallback_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let (first, calls) = fake(None, true);
        let (miss, _) = fake(None, false);
        let service =
            LyricsService::with_providers(vec![first, miss], Some(LyricsCache::new(dir.path())));
        assert!(service.lookup(&track()).is_err());
        assert!(service.lookup(&track()).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        let (first, _) = fake(None, true);
        let (fallback, calls) = fake(Some(plain()), false);
        let service = LyricsService::with_providers(vec![first, fallback], None);
        assert_eq!(
            service.lookup(&track()).unwrap().quality,
            LyricsQuality::Plain
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn local_overrides_cache_and_rereads_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.lrc");
        fs::write(&file, "[00:01]<00:01>Привіт<00:02>").unwrap();
        let (provider, calls) = fake(Some(plain()), false);
        let service = LyricsService::with_providers(
            vec![provider],
            Some(LyricsCache::new(dir.path().join("cache"))),
        )
        .with_local_path(&file);
        assert_eq!(
            service.lookup(&track()).unwrap().quality,
            LyricsQuality::Word
        );
        fs::write(&file, "[00:01]Changed").unwrap();
        assert_eq!(service.lookup(&track()).unwrap().lines[0].text, "Changed");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let json = dir.path().join("lyrics.json");
        fs::write(&json, WORDS_JSON).unwrap();
        assert_eq!(load_local(&json).unwrap().quality, LyricsQuality::Word);
        fs::write(&file, [0xff, 0xfe]).unwrap();
        assert!(load_local(&file).is_err());
    }

    #[test]
    fn search_requires_exact_title_artist_and_duration_tolerance() {
        let mut record = LrclibRecord {
            track_name: track().title,
            artist_name: track().artist,
            duration: 13.9,
            ..Default::default()
        };
        assert!(matches_track(&record, &track()));
        record.duration = 14.1;
        assert!(!matches_track(&record, &track()));
        record.duration = 12.0;
        record.track_name.push_str(" live");
        assert!(!matches_track(&record, &track()));
    }

    // Only a loopback fake server is used; CI never contacts LRCLIB.
    fn server(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 65536);
                }
                requests.push(String::from_utf8(request).unwrap());
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        (origin, handle)
    }
    fn response(status: &str, body: &str, extra: &str) -> String {
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len())
    }

    #[test]
    fn http_404_search_fallback_sends_bounded_identified_encoded_requests() {
        let body = serde_json::json!([{"trackName":track().title,"artistName":track().artist,"duration":12,"lyricsfile":WORDS_YAML}]).to_string();
        let (origin, handle) = server(vec![
            response("404 Not Found", "{}", ""),
            response("200 OK", &body, ""),
        ]);
        let provider = LrclibProvider::with_base_url(&origin).unwrap();
        let doc = provider.lookup(&track()).unwrap().unwrap();
        assert_eq!(doc.quality, LyricsQuality::Word);
        let requests = handle.join().unwrap();
        assert!(requests[0].starts_with("GET /api/get?"));
        assert!(requests[1].starts_with("GET /api/search?"));
        assert!(requests[0].contains("duration=12"));
        assert!(!requests[1].contains("duration="));
        assert!(requests[0]
            .to_lowercase()
            .contains(&format!("user-agent: {}", USER_AGENT.to_lowercase())));
        assert!(requests[0].contains("%D0"));
    }

    #[test]
    fn http_rate_limit_sets_cooldown_without_retries_or_negative_cache() {
        let (origin, handle) = server(vec![response(
            "429 Too Many Requests",
            "not-json",
            "Retry-After: 3600\r\n",
        )]);
        let provider = LrclibProvider::with_base_url(&origin).unwrap();
        assert!(provider
            .lookup(&track())
            .unwrap_err()
            .to_string()
            .contains("429"));
        assert!(provider
            .lookup(&track())
            .unwrap_err()
            .to_string()
            .contains("cooldown"));
        assert_eq!(handle.join().unwrap().len(), 1);
    }

    #[test]
    fn http_rejects_oversize_invalid_json_non_json_and_server_failure() {
        for raw in [
            response("200 OK", "not JSON", ""),
            response("500 Server Error", "{}", ""),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into(),
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_DOCUMENT_BYTES + 1),
        ] {
            let (origin, handle) = server(vec![raw]);
            assert!(LrclibProvider::with_base_url(&origin).unwrap().lookup(&track()).is_err());
            assert_eq!(handle.join().unwrap().len(), 1);
        }
        assert!(read_bounded(&b"12345"[..], 4).is_err());
    }
}
