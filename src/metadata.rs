use crate::playback::Track;
/// Conservative catalog query normalization; displayed source metadata is kept.
/// Do not remove meaningful qualifiers such as Live, Acoustic or Remix.
pub fn lyrics_query(track: &Track) -> Track {
    let mut t = track.clone();
    let mut title = t.title.trim().to_owned();
    for separator in [" - ", " – ", " — "] {
        let prefix = format!("{}{separator}", t.artist.trim());
        if title.to_lowercase().starts_with(&prefix.to_lowercase()) && !t.artist.trim().is_empty() {
            title = title[prefix.len()..].to_owned();
            break;
        }
    }
    while let Some(open) = title.rfind(['(', '[']) {
        let suffix = title[open..].to_lowercase();
        if (suffix.ends_with(')') || suffix.ends_with(']'))
            && [
                "official video",
                "official music video",
                "official audio",
                "lyrics video",
                "lyric video",
                "4k remaster",
                "hd remaster",
            ]
            .iter()
            .any(|label| suffix[1..suffix.len() - 1].trim() == *label)
        {
            title.truncate(open);
            title = title.trim().to_owned();
        } else {
            break;
        }
    }
    if !title.is_empty() {
        t.title = title;
    }
    t
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn promotional_labels_do_not_hide_real_lyrics() {
        let t = Track {
            artist: "Rick Astley".into(),
            title: "Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)".into(),
            ..Default::default()
        };
        assert_eq!(lyrics_query(&t).title, "Never Gonna Give You Up");
    }
    #[test]
    fn musical_version_qualifiers_are_preserved() {
        let t = Track {
            artist: "Example".into(),
            title: "Track (Live)".into(),
            ..Default::default()
        };
        assert_eq!(lyrics_query(&t).title, t.title);
    }
}
