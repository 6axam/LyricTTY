use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq)]
pub struct LyricWord {
    pub text: String,
    pub start: f32,
    pub end: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    pub start: f32,
    pub end: f32,
    pub words: Vec<LyricWord>,
    pub text: String,
}

pub fn parse_lrc(input: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw in input.lines() {
        let Some(close) = raw.find(']') else { continue };
        let tag = &raw[1..close];
        let Some((m, s)) = tag.split_once(':') else {
            continue;
        };
        let Some(start) = m
            .parse::<f32>()
            .ok()
            .zip(s.parse::<f32>().ok())
            .map(|(m, s)| m * 60.0 + s)
        else {
            continue;
        };
        let text = raw[close + 1..].trim().to_string();
        if !text.is_empty() {
            lines.push(LyricLine {
                start,
                end: start + 4.0,
                words: Vec::new(),
                text,
            });
        }
    }
    lines.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(Ordering::Equal));
    for i in 0..lines.len().saturating_sub(1) {
        lines[i].end = lines[i + 1].start;
    }
    lines
}

pub fn active_line(lines: &[LyricLine], position: f32) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| position >= l.start)
        .map(|(i, _)| i)
}

pub fn active_word(line: &LyricLine, position: f32) -> Option<usize> {
    line.words
        .iter()
        .position(|w| position >= w.start && position < w.end)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_and_times_lines() {
        let x = parse_lrc("[00:01.20]one\n[00:03.00]two");
        assert_eq!(x.len(), 2);
        assert!((x[0].end - 3.0).abs() < 0.01);
    }
    #[test]
    fn selects_active_line() {
        let x = parse_lrc("[00:01.00]one\n[00:03.00]two");
        assert_eq!(active_line(&x, 3.2), Some(1));
    }
}
