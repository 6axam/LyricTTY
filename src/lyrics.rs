//! Renderer models and a tolerant, UTF-8 safe LRC/enhanced-LRC reader.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LyricWord {
    pub text: String,
    pub start: f32,
    pub end: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LyricLine {
    pub start: f32,
    pub end: f32,
    pub words: Vec<LyricWord>,
    pub text: String,
}

/// Read real line and `<mm:ss.xxx>` word cues. Ordinary LRC never gets
/// synthetic words. Empty cues terminate the previous line (instrumental gaps).
/// A final line without an endpoint has a four-second display window; that
/// display policy is never used to manufacture a word end.
pub fn parse_lrc(input: &str) -> Vec<LyricLine> {
    // Repeated timestamp tags can expand a small input into many copies of a
    // long line. Bound expanded text as well as the source document.
    if input.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    // As in FFmpeg/OpenLyrics, a positive LRC offset displays lyrics earlier.
    let offset = -(input
        .lines()
        .filter_map(|raw| {
            raw.trim()
                .trim_start_matches('\u{feff}')
                .strip_prefix("[offset:")?
                .split_once(']')?
                .0
                .trim()
                .parse::<i64>()
                .ok()
        })
        .next_back()
        .unwrap_or(0) as f64)
        / 1000.0;
    let mut lines = Vec::new();
    let mut expanded_bytes = 0usize;
    let mut expanded_words = 0usize;
    for raw in input.lines() {
        let mut rest = raw.trim().trim_start_matches('\u{feff}');
        let mut starts = Vec::new();
        while let Some(tagged) = rest.strip_prefix('[') {
            let Some((tag, tail)) = tagged.split_once(']') else {
                break;
            };
            let Some(time) = timestamp(tag) else { break };
            starts.push(time);
            if starts.len() > 20_000 {
                return Vec::new();
            }
            rest = tail.trim_start();
        }
        let Some(&base) = starts.first() else {
            continue;
        };
        let (text, words) = enhanced_text(rest.trim());
        for start in starts {
            expanded_bytes = expanded_bytes.saturating_add(text.len().saturating_mul(2));
            expanded_words = expanded_words.saturating_add(words.len());
            if lines.len() >= 20_000 || expanded_bytes > 8 * 1024 * 1024 || expanded_words > 100_000
            {
                return Vec::new();
            }
            let shift = start - base + offset;
            let words = words
                .iter()
                .map(|(text, begin, end)| LyricWord {
                    text: text.clone(),
                    start: seconds(begin + shift),
                    end: end.map(|end| seconds(end + shift)).unwrap_or(-1.0),
                })
                .collect();
            lines.push(LyricLine {
                start: seconds(start + offset),
                end: 0.0,
                words,
                text: text.clone(),
            });
        }
    }
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut group = 0;
    while group < lines.len() {
        let start = lines[group].start;
        let mut next = group + 1;
        while next < lines.len() && lines[next].start == start {
            next += 1;
        }
        let boundary = lines.get(next).map(|line| line.start);
        for line in &mut lines[group..next] {
            let real_word_end = line
                .words
                .last()
                .filter(|w| w.end >= w.start)
                .map(|w| w.end);
            line.end = boundary.or(real_word_end).unwrap_or(start + 4.0).max(start);
            for word in &mut line.words {
                if word.end < 0.0 {
                    word.end = boundary.unwrap_or(word.start).max(word.start);
                }
            }
            if line
                .words
                .iter()
                .any(|w| w.start < line.start || w.end < w.start)
                || line.words.windows(2).any(|w| w[0].start > w[1].start)
            {
                line.words.clear();
            }
        }
        group = next;
    }
    lines.retain(|line| !line.text.is_empty());
    lines
}

fn seconds(value: f64) -> f32 {
    value.max(0.0).min(f32::MAX as f64 / 2.0) as f32
}

fn timestamp(tag: &str) -> Option<f64> {
    let parts: Vec<_> = tag.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let integer = |s: &str| -> Option<f64> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse::<u64>().ok().map(|x| x as f64)
    };
    let seconds_part = parts.last()?;
    let (whole, fraction) = seconds_part.split_once('.').unwrap_or((seconds_part, ""));
    let whole = integer(whole)?;
    if whole >= 60.0
        || (!fraction.is_empty() && !fraction.bytes().all(|c| c.is_ascii_digit()))
        || (seconds_part.contains('.') && fraction.is_empty())
    {
        return None;
    }
    let fraction = if fraction.is_empty() {
        0.0
    } else {
        format!("0.{fraction}").parse::<f64>().ok()?
    };
    let minutes = integer(parts[parts.len() - 2])?;
    let hours = if parts.len() == 3 {
        if minutes >= 60.0 {
            return None;
        }
        integer(parts[0])?
    } else {
        0.0
    };
    Some(hours * 3600.0 + minutes * 60.0 + whole + fraction)
}

type TimedSegment = (String, f64, Option<f64>);

fn enhanced_text(input: &str) -> (String, Vec<TimedSegment>) {
    let mut tags = Vec::new();
    let mut open = None;
    for (index, c) in input.char_indices() {
        if c == '<' {
            open = Some(index);
        }
        if c == '>' {
            if let Some(start) = open.take() {
                if let Some(time) = timestamp(&input[start + 1..index]) {
                    tags.push((start, index + 1, time));
                }
            }
        }
    }
    if tags.is_empty() {
        return (input.to_owned(), Vec::new());
    }
    let prefix = &input[..tags[0].0];
    let mut text = prefix.to_owned();
    let mut words = Vec::new();
    for (i, &(_, after, time)) in tags.iter().enumerate() {
        let next = tags.get(i + 1);
        let segment = &input[after..next.map(|x| x.0).unwrap_or(input.len())];
        text.push_str(segment);
        if !segment.is_empty() {
            words.push((segment.to_owned(), time, next.map(|x| x.2)));
        }
    }
    // The renderer displays words instead of line.text. Use them only when
    // every displayed character has a real timing cue.
    if !prefix.is_empty() || words.iter().map(|w| w.0.as_str()).collect::<String>() != text {
        words.clear();
    }
    (text, words)
}

/// Half-open [start, end) intervals; gaps/non-finite positions have no active
/// line. For overlapping vocals the last matching line takes precedence.
pub fn active_line(lines: &[LyricLine], position: f32) -> Option<usize> {
    if !position.is_finite() {
        return None;
    }
    lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, line)| !line.text.is_empty() && position >= line.start && position < line.end)
        .map(|(i, _)| i)
}

pub fn active_word(line: &LyricLine, position: f32) -> Option<usize> {
    if !position.is_finite() || position < line.start || position >= line.end {
        return None;
    }
    line.words
        .iter()
        .enumerate()
        .rev()
        .find(|(_, word)| position >= word.start && position < word.end)
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_and_times_lines() {
        let lines = parse_lrc("[00:01.20]one\n[00:03.00]two");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].end, 3.0);
        assert!(lines.iter().all(|l| l.words.is_empty()));
    }
    #[test]
    fn enhanced_cyrillic_preserves_spaces_and_real_endpoints() {
        let lines = parse_lrc("[00:01.00]<00:01.00>Привіт, <00:01.50>світе 🌍<00:02.00>");
        assert_eq!(lines[0].text, "Привіт, світе 🌍");
        assert_eq!(lines[0].words[0].text, "Привіт, ");
        assert_eq!(active_word(&lines[0], 1.499), Some(0));
        assert_eq!(active_word(&lines[0], 1.5), Some(1));
        assert_eq!(active_word(&lines[0], 2.0), None);
        assert_eq!(active_line(&lines, 2.0), None);
    }
    #[test]
    fn multiple_tags_offsets_and_blank_cues() {
        let lines = parse_lrc("\u{feff}[ar:Artist]\n[00:01.00][00:03.00]<00:01.00>Так<00:01.50>\n[00:02.00]\n[offset:+500]");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start, 0.5);
        assert_eq!(lines[0].end, 1.5);
        assert_eq!(lines[1].words[0].start, 2.5);
        assert_eq!(lines[1].words[0].end, 3.0);
        assert_eq!(active_line(&lines, 1.5), None);
        assert_eq!(active_line(&lines, 2.5), Some(1));
    }
    #[test]
    fn malformed_unicode_and_nonfinite_timestamps_never_panic() {
        for input in [
            "я]",
            "]",
            "🌍[",
            "[",
            "[]",
            "[00:NaN]x",
            "[inf:01]x",
            "[-1:02]x",
            "[00:60]x",
            "[0:1.]x",
            "[0:1e2]x",
            "[0:1.2.3]x",
        ] {
            assert!(parse_lrc(input).is_empty(), "{input}");
        }
        let lines = parse_lrc("[00:01]🌍 <broken> тест\n[00:03]end");
        assert_eq!(lines[0].text, "🌍 <broken> тест");
        assert_eq!(active_line(&lines, f32::NAN), None);
        assert_eq!(active_line(&lines, f32::INFINITY), None);
    }
    #[test]
    fn half_open_boundaries_and_overlaps() {
        let lines = parse_lrc("[00:01]one\n[00:03]two\n[00:05]");
        assert_eq!(active_line(&lines, 0.99), None);
        assert_eq!(active_line(&lines, 1.0), Some(0));
        assert_eq!(active_line(&lines, 3.0), Some(1));
        assert_eq!(active_line(&lines, 5.0), None);
        let mut overlapping = lines.clone();
        overlapping[0].end = 8.0;
        assert_eq!(active_line(&overlapping, 6.0), Some(0));
    }
    #[test]
    fn no_invented_final_word_end_or_timing_for_prefix() {
        let lines = parse_lrc("[00:01]<00:01>один <00:02>два");
        assert_eq!(lines[0].words[1].end, 2.0);
        assert_eq!(active_word(&lines[0], 2.5), None);
        let lines = parse_lrc("[00:01]untimed <00:02>word<00:03>");
        assert_eq!(lines[0].text, "untimed word");
        assert!(lines[0].words.is_empty());
    }

    #[test]
    fn negative_offset_delays_and_positive_offset_clamps_at_zero() {
        assert_eq!(parse_lrc("[offset:-500]\n[00:01]later")[0].start, 1.5);
        assert_eq!(parse_lrc("[offset:2000]\n[00:01]earlier")[0].start, 0.0);
    }
}
