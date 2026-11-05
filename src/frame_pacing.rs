use std::time::{Duration, Instant};

/// A frame deadline, independent of input event frequency and rendering cost.
///
/// Render only when `wait(now).is_zero()`. Call `presented` after rendering,
/// even if rendering was slow. On waking from idle, call `reset` once to make
/// the first frame immediate; do not reset for ordinary input events.
pub(crate) struct FramePacer {
    deadline: Instant,
    period: Duration,
}

impl FramePacer {
    pub(crate) fn new(now: Instant, fps: u32) -> Self {
        Self {
            deadline: now,
            period: frame_period(fps),
        }
    }

    /// Time until the next frame; zero means a frame is due.
    pub(crate) fn wait(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    /// Advance from the frame's scheduled start, not its completion time.
    /// Missed frames are dropped in constant time, leaving a future deadline.
    /// `now` must be the completion time of a frame that was due.
    pub(crate) fn presented(&mut self, now: Instant, fps: u32) {
        self.period = frame_period(fps);
        let elapsed = now.saturating_duration_since(self.deadline);
        let period_ns = self.period.as_nanos();
        let remainder = (elapsed.as_nanos() % period_ns) as u64;
        self.deadline = now + (self.period - Duration::from_nanos(remainder));
    }

    /// Forget the old cadence when transitioning from idle to active.
    pub(crate) fn reset(&mut self, now: Instant) {
        self.deadline = now;
    }
}

fn frame_period(fps: u32) -> Duration {
    Duration::from_nanos(1_000_000_000 / u64::from(fps.clamp(10, 60)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_is_immediate_and_render_time_does_not_shift_cadence() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(start, 60);
        let period = Duration::from_nanos(16_666_666);
        assert_eq!(pacer.wait(start), Duration::ZERO);
        pacer.presented(start + Duration::from_millis(5), 60);
        assert_eq!(pacer.wait(start), period);
        assert_eq!(
            pacer.wait(start + period - Duration::from_nanos(1)),
            Duration::from_nanos(1)
        );
        assert_eq!(pacer.wait(start + period), Duration::ZERO);
        pacer.presented(start + period + Duration::from_millis(3), 60);
        assert_eq!(pacer.wait(start), period * 2);
    }

    #[test]
    fn slow_frames_drop_missed_boundaries_without_catch_up() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(start, 60);
        let period = frame_period(60);
        let finished = start + period * 1_000_000 + Duration::from_nanos(7);
        pacer.presented(finished, 60);
        assert_eq!(pacer.wait(finished), period - Duration::from_nanos(7));
        let next = finished + pacer.wait(finished);
        pacer.presented(next + period * 2, 60);
        assert_eq!(pacer.wait(next + period * 2), period);
    }

    #[test]
    fn fps_changes_use_scheduled_start_and_clamp_both_limits() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(start, 0);
        assert_eq!(pacer.period, Duration::from_millis(100));
        pacer.presented(start + Duration::from_millis(5), 30);
        assert_eq!(pacer.wait(start), Duration::from_nanos(33_333_333));
        let next = start + pacer.wait(start);
        pacer.presented(next + Duration::from_millis(2), u32::MAX);
        assert_eq!(pacer.wait(next), Duration::from_nanos(16_666_666));
        let next = next + pacer.wait(next);
        pacer.presented(next, 1);
        assert_eq!(pacer.wait(next), Duration::from_millis(100));
    }

    #[test]
    fn idle_reset_starts_a_fresh_immediate_cadence() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(start, 60);
        pacer.presented(start, 60);
        let wake = start + Duration::from_secs(10) + Duration::from_millis(7);
        pacer.reset(wake);
        assert_eq!(pacer.wait(wake), Duration::ZERO);
        pacer.presented(wake + Duration::from_millis(4), 60);
        assert_eq!(pacer.wait(wake), frame_period(60));
    }

    #[test]
    fn input_polling_does_not_move_the_deadline() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(start, 10);
        pacer.presented(start, 10);
        for ms in 1..100 {
            assert_eq!(
                pacer.wait(start + Duration::from_millis(ms)),
                Duration::from_millis(100 - ms)
            );
        }
    }
}
