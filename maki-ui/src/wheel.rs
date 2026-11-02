//! Turns mouse wheel events into line counts. The velocity curve follows
//! OpenCode's `MacOSScrollAccel`; the gesture handling is from pi's
//! `WheelScrollAccelerator`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use maki_config::MouseScrollLines;

/// Events closer than this belong to one physical notch (Ghostty emits them
/// ~4 ms apart) or come from a high-resolution source, so they do not speed up.
const BURST_GAP_MS: f64 = 6.0;
/// A longer pause ends the gesture.
const GESTURE_GAP: Duration = Duration::from_millis(150);
const GAP_HISTORY: usize = 3;
/// The average gap that gives a velocity of 1.
const REFERENCE_GAP_MS: f64 = 100.0;
const CURVE_AMPLITUDE: f64 = 0.8;
const CURVE_TAU: f64 = 3.0;
const MAX_LINES_PER_EVENT: f64 = 6.0;

#[derive(Default)]
pub(crate) struct WheelScroll {
    last: Option<(Instant, bool)>,
    gaps_ms: VecDeque<f64>,
    carry: f64,
}

impl WheelScroll {
    /// Signed line delta for events drained together: positive scrolls up.
    pub(crate) fn delta(
        &mut self,
        setting: MouseScrollLines,
        ups: u32,
        downs: u32,
        now: Instant,
    ) -> i32 {
        let up = self.lines(setting, true, ups, now) as i32;
        let down = self.lines(setting, false, downs, now) as i32;
        up - down
    }

    /// `count` events arrived since the last call; the time since then is
    /// spread over them, so a slow frame does not make a fast spin look slow.
    fn lines(&mut self, setting: MouseScrollLines, up: bool, count: u32, now: Instant) -> u32 {
        match setting {
            MouseScrollLines::Fixed(lines) => lines * count,
            MouseScrollLines::Auto if count == 0 => 0,
            MouseScrollLines::Auto => self.auto_lines(up, count, now),
        }
    }

    fn auto_lines(&mut self, up: bool, count: u32, now: Instant) -> u32 {
        let gap = self
            .last
            .filter(|&(_, last_up)| last_up == up)
            .map(|(at, _)| now.saturating_duration_since(at))
            .filter(|gap| *gap <= GESTURE_GAP);
        let Some(gap) = gap else {
            self.last = Some((now, up));
            self.gaps_ms.clear();
            self.carry = 0.0;
            return count;
        };
        let gap_ms = gap.as_secs_f64() * 1000.0 / f64::from(count);
        if gap_ms < BURST_GAP_MS {
            return count;
        }
        self.last = Some((now, up));
        (0..count).map(|_| self.accelerated_lines(gap_ms)).sum()
    }

    fn accelerated_lines(&mut self, gap_ms: f64) -> u32 {
        if self.gaps_ms.len() == GAP_HISTORY {
            self.gaps_ms.pop_front();
        }
        self.gaps_ms.push_back(gap_ms);
        let average = self.gaps_ms.iter().sum::<f64>() / self.gaps_ms.len() as f64;
        let velocity = REFERENCE_GAP_MS / average;
        let multiplier =
            (1.0 + CURVE_AMPLITUDE * ((velocity / CURVE_TAU).exp() - 1.0)).min(MAX_LINES_PER_EVENT);
        let lines = multiplier + self.carry;
        let whole = lines.floor();
        self.carry = lines - whole;
        whole as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    const UP: bool = true;

    fn spin(wheel: &mut WheelScroll, start: Instant, offsets_ms: &[u64], up: bool) -> Vec<u32> {
        offsets_ms
            .iter()
            .map(|ms| {
                wheel.lines(
                    MouseScrollLines::Auto,
                    up,
                    1,
                    start + Duration::from_millis(*ms),
                )
            })
            .collect()
    }

    #[test]
    fn fixed_ignores_timing() {
        let mut wheel = WheelScroll::default();
        let start = Instant::now();
        let deltas: Vec<i32> = [0, 10, 20, 1000]
            .iter()
            .map(|ms| {
                wheel.delta(
                    MouseScrollLines::Fixed(3),
                    2,
                    0,
                    start + Duration::from_millis(*ms),
                )
            })
            .collect();
        assert_eq!(deltas, [6, 6, 6, 6]);
    }

    #[test_case(&[0, 150, 300, 450], &[1, 1, 1, 1] ; "slow_notches_stay_precise")]
    #[test_case(&[0, 100, 200, 300], &[1, 1, 1, 1] ; "gesture_gap_ends_the_streak")]
    #[test_case(&[0, 50, 100, 150], &[1, 1, 2, 2] ; "moderate_spin")]
    #[test_case(&[0, 20, 40, 60], &[1, 4, 4, 5] ; "fast_spin")]
    #[test_case(&[0, 10, 20, 30], &[1, 6, 6, 6] ; "capped")]
    #[test_case(&[0, 4, 100, 104], &[1, 1, 1, 1] ; "burst_from_one_notch")]
    fn auto_follows_velocity(offsets_ms: &[u64], expected: &[u32]) {
        let mut wheel = WheelScroll::default();
        assert_eq!(spin(&mut wheel, Instant::now(), offsets_ms, UP), expected);
    }

    #[test]
    fn auto_resets_on_direction_change_and_pause() {
        let mut wheel = WheelScroll::default();
        let start = Instant::now();
        assert_eq!(spin(&mut wheel, start, &[0, 20, 40], UP), [1, 4, 4]);
        assert_eq!(spin(&mut wheel, start, &[60], !UP), [1]);
        assert_eq!(spin(&mut wheel, start, &[500, 520], !UP), [1, 4]);
    }

    #[test]
    fn batch_is_measured_by_the_time_each_event_took() {
        let start = Instant::now();
        let mut one_by_one = WheelScroll::default();
        let one_by_one_total: u32 = spin(&mut one_by_one, start, &[0, 20, 40, 60, 80], UP)
            .iter()
            .sum();

        let mut batched = WheelScroll::default();
        batched.lines(MouseScrollLines::Auto, UP, 1, start);
        let batched_total = batched.lines(
            MouseScrollLines::Auto,
            UP,
            4,
            start + Duration::from_millis(80),
        );

        assert_eq!(batched_total, one_by_one_total - 1);
    }

    #[test]
    fn delta_combines_both_directions() {
        let mut wheel = WheelScroll::default();
        assert_eq!(wheel.delta(MouseScrollLines::Auto, 3, 1, Instant::now()), 2);
    }
}
