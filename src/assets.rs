use crate::{
    artwork::{Artwork, ArtworkProvider},
    config::Config,
    content::LyricsContent,
    playback::Track,
    providers::{load_local, LyricsCache, LyricsDocument, LyricsService},
};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};
pub enum AssetEvent {
    Lyrics { key: String, content: LyricsContent },
    Artwork { key: String, artwork: Artwork },
}
struct Request {
    key: String,
    track: Track,
    local: Option<PathBuf>,
    demo: bool,
}
pub struct AssetService {
    requests: Sender<Request>,
    pub events: Receiver<AssetEvent>,
}
impl AssetService {
    pub fn start(config: Config, cache: PathBuf) -> Self {
        let (requests, rx) = mpsc::channel::<Request>();
        let (tx, events) = mpsc::channel();
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
                if tx
                    .send(AssetEvent::Lyrics {
                        key: r.key,
                        content,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self { requests, events }
    }
    pub fn request(&self, key: String, track: Track, local: Option<PathBuf>, demo: bool) {
        let _ = self.requests.send(Request {
            key,
            track,
            local,
            demo,
        });
    }
}
