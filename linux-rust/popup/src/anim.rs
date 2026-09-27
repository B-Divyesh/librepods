// Easing curves and a tiny time-based tween.
//
// Adapted from podctl's `src/bin/popup/anim.rs`
// (https://github.com/Rockykln/podctl, MIT OR Apache-2.0,
// © 2026 pods contributors). The frame-counting `Slide` iterator became a
// wall-clock `Tween` so that frames can be paced by the compositor's frame
// callbacks instead of `sleep(16ms)` loops.

use std::time::{Duration, Instant};

pub type Ease = fn(f32) -> f32;

pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

pub fn ease_in_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t
}

/// Ease-out with a small overshoot, a cheap stand-in for the damped spring
/// UIKit uses for sheets and cards.
pub fn ease_out_back(t: f32) -> f32 {
    const S: f32 = 1.05;
    let t = t.clamp(0.0, 1.0) - 1.0;
    1.0 + (S + 1.0) * t * t * t + S * t * t
}

/// A value moving from `from` to `to` over `dur`, starting at `start`
/// (which may lie in the future to express a delay).
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur: Duration,
    ease: Ease,
}

impl Tween {
    pub fn new(from: f32, to: f32, start: Instant, dur: Duration, ease: Ease) -> Self {
        Tween {
            from,
            to,
            start,
            dur,
            ease,
        }
    }

    pub fn progress(&self, now: Instant) -> f32 {
        if self.dur.is_zero() {
            return 1.0;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32();
        t.clamp(0.0, 1.0)
    }

    pub fn value(&self, now: Instant) -> f32 {
        let p = self.progress(now);
        if p >= 1.0 {
            return self.to;
        }
        self.from + (self.to - self.from) * (self.ease)(p)
    }

    pub fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }

    /// Re-aim at a new target, continuing from wherever the value is now.
    pub fn retarget(&mut self, to: f32, now: Instant, dur: Duration, ease: Ease) {
        if (to - self.to).abs() < f32::EPSILON {
            return;
        }
        *self = Tween::new(self.value(now), to, now, dur, ease);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_endpoints() {
        for f in [ease_out_cubic, ease_in_cubic, ease_out_back] {
            assert!(f(0.0).abs() < 1e-5);
            assert!((f(1.0) - 1.0).abs() < 1e-5);
        }
        assert!(ease_out_back(0.8) > 1.0, "back-ease must overshoot");
    }

    #[test]
    fn tween_hits_both_ends() {
        let t0 = Instant::now();
        let tw = Tween::new(0.0, 10.0, t0, Duration::from_millis(100), ease_out_cubic);
        assert!(tw.value(t0).abs() < 1e-5);
        assert!((tw.value(t0 + Duration::from_millis(200)) - 10.0).abs() < 1e-5);
        assert!(tw.done(t0 + Duration::from_millis(100)));
        assert!(!tw.done(t0 + Duration::from_millis(50)));
    }

    #[test]
    fn delayed_start_holds_from() {
        let t0 = Instant::now();
        let tw = Tween::new(2.0, 4.0, t0 + Duration::from_millis(50), Duration::from_millis(50), ease_in_cubic);
        assert!((tw.value(t0) - 2.0).abs() < 1e-5);
    }
}
