use crate::lyrics::LyricLine;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrumentalPhase {
    Lyrics,
    EnteringInstrumental,
    Instrumental,
    LeavingInstrumental,
}
#[derive(Debug)]
pub struct InstrumentalState {
    pub phase: InstrumentalPhase,
    pub intensity: f32,
}
impl Default for InstrumentalState {
    fn default() -> Self {
        Self {
            phase: InstrumentalPhase::Lyrics,
            intensity: 0.0,
        }
    }
}
impl InstrumentalState {
    pub fn update(
        &mut self,
        lines: &[LyricLine],
        position: f32,
        duration: f32,
        threshold: f32,
        fade: f32,
        entire_track: bool,
    ) {
        self.intensity = 0.0;
        if entire_track {
            self.phase = InstrumentalPhase::Instrumental;
            self.intensity = 1.0;
            return;
        }
        // Line-level LRC gives starts, not vocal ends. Treat the last four seconds
        // after a line as potentially vocal; explicit word ends override the estimate.
        let previous = lines.iter().rev().find(|l| l.start <= position);
        let next = lines
            .iter()
            .find(|l| l.start > position)
            .map(|l| l.start)
            .unwrap_or(duration);
        let end = previous
            .map(|l| {
                l.words
                    .last()
                    .map(|w| w.end)
                    .unwrap_or(l.end.min(l.start + 4.0))
            })
            .unwrap_or(0.0);
        if !lines.is_empty() && next - end >= threshold && position >= end && position <= next {
            self.intensity = ((position - end) / fade)
                .min((next - position) / fade)
                .clamp(0.0, 1.0);
            self.phase = if position < end + fade {
                InstrumentalPhase::EnteringInstrumental
            } else if position > next - fade {
                InstrumentalPhase::LeavingInstrumental
            } else {
                InstrumentalPhase::Instrumental
            };
        } else {
            self.phase = InstrumentalPhase::Lyrics;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gap_enters_holds_and_leaves_then_returns_to_lyrics() {
        let lines = crate::lyrics::parse_lrc("[00:00]hello\n[00:30]world");
        let mut s = InstrumentalState::default();
        for (t, phase) in [
            (2.0, InstrumentalPhase::Lyrics),
            (5.0, InstrumentalPhase::EnteringInstrumental),
            (15.0, InstrumentalPhase::Instrumental),
            (29.0, InstrumentalPhase::LeavingInstrumental),
            (30.0, InstrumentalPhase::Lyrics),
        ] {
            s.update(&lines, t, 60.0, 8.0, 2.0, false);
            assert_eq!(s.phase, phase, "{t}");
        }
    }
    #[test]
    fn no_lyrics_is_not_claimed_to_be_instrumental() {
        let mut s = InstrumentalState::default();
        s.update(&[], 10.0, 200.0, 8.0, 2.0, false);
        assert_eq!(s.phase, InstrumentalPhase::Lyrics);
    }
}
