//! Motion: how MacBridge's own surfaces move, in one place. Durations come from a small set of
//! tokens (as a design system names them), curves are Apple's standard ones or a spring, and
//! every animation can be interrupted: a new target starts from where the value is now, with
//! the speed it has now, so nothing jumps. With reduced motion (Windows' "Animation effects"
//! off, or the setting) movement is replaced by a short fade or happens at once.
//!
//! Nothing here animates over the live picture of a Mac window: surfaces animate themselves
//! (their own small layered windows), never the video.

use std::time::{Duration, Instant};

/// Duration tokens (milliseconds): the short end for small things, the long end for large ones.
pub mod tokens {
    /// a button or row pressed
    pub const PRESS: (u32, u32) = (80, 160);
    /// a popover, a tooltip, the navigation ball's menu
    pub const POPOVER: (u32, u32) = (120, 200);
    /// a menu or the search palette
    pub const MENU: (u32, u32) = (160, 240);
    /// a window appearing, moving or changing size
    pub const WINDOW: (u32, u32) = (200, 360);
    /// a change of mode (fullscreen, the Mac Desktop, a state of the connect window)
    pub const MODE: (u32, u32) = (250, 420);

    /// A duration within a token's range: `t` 0 (smallest) .. 1 (largest).
    pub fn pick(token: (u32, u32), t: f32) -> std::time::Duration {
        let t = t.clamp(0.0, 1.0);
        std::time::Duration::from_millis((token.0 as f32 + (token.1 - token.0) as f32 * t).round() as u64)
    }
}

/// How a value moves from its start to its end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    Linear,
    /// ease in and out (Apple's default, 0.42 0 0.58 1)
    Standard,
    /// fast start, gentle stop: things arriving (0 0 0.2 1)
    Decelerate,
    /// gentle start: things leaving (0.4 0 1 1)
    Accelerate,
    /// a spring: `response` (seconds per oscillation) and `damping` (1 = no overshoot)
    Spring { response: f32, damping: f32 },
}

impl Curve {
    /// A window or a panel arriving: a spring that settles with the slightest overshoot.
    pub const ARRIVE: Curve = Curve::Spring { response: 0.32, damping: 0.86 };
    /// Something small reacting (a press, a toggle): quick, no overshoot.
    pub const SNAP: Curve = Curve::Spring { response: 0.2, damping: 1.0 };
}

/// The cubic Bézier easing (x1, y1, x2, y2) at progress `x` (0..1): y.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, t: f32| {
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    };
    // solve x(t) = x by Newton, then bisection when the slope is flat
    let mut t = x;
    for _ in 0..8 {
        let err = bez(x1, x2, t) - x;
        let u = 1.0 - t;
        let d = 3.0 * u * u * x1 + 6.0 * u * t * (x2 - x1) + 3.0 * t * t * (1.0 - x2);
        if err.abs() < 1e-5 {
            return bez(y1, y2, t);
        }
        if d.abs() < 1e-6 {
            break;
        }
        t = (t - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) * 0.5;
        if bez(x1, x2, mid) < x {
            lo = mid
        } else {
            hi = mid
        }
    }
    bez(y1, y2, (lo + hi) * 0.5)
}

/// A damped spring from 0 to 1 that starts with velocity `v0` (units per second), at time `t`
/// seconds: (position, velocity).
pub fn spring(response: f32, damping: f32, v0: f32, t: f32) -> (f32, f32) {
    let w0 = std::f32::consts::TAU / response.max(0.01);
    let z = damping.clamp(0.05, 2.0);
    // x(t) = 1 - e(t): e is the remaining distance, e(0) = 1, e'(0) = -v0
    let (e, de) = if z < 0.999 {
        let wd = w0 * (1.0 - z * z).sqrt();
        let a = 1.0;
        let b = (z * w0 * a - v0) / wd;
        let k = (-z * w0 * t).exp();
        let (s, c) = (wd * t).sin_cos();
        let e = k * (a * c + b * s);
        let de = k * ((-z * w0) * (a * c + b * s) + (-a * wd * s + b * wd * c));
        (e, de)
    } else {
        // critically damped
        let a = 1.0;
        let b = w0 * a - v0;
        let k = (-w0 * t).exp();
        let e = k * (a + b * t);
        let de = k * (b - w0 * (a + b * t));
        (e, de)
    };
    (1.0 - e, -de)
}

/// How long a spring takes to settle (within 0.1 % of its end).
pub fn spring_settles(response: f32, damping: f32) -> f32 {
    let mut t = 0.0;
    while t < 5.0 {
        let (x, v) = spring(response, damping, 0.0, t);
        if (1.0 - x).abs() < 0.001 && v.abs() < 0.01 {
            return t;
        }
        t += 0.004;
    }
    5.0
}

/// One animated value. `retarget` changes where it goes without a jump.
#[derive(Debug, Clone)]
pub struct Anim {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
    curve: Curve,
    /// speed at the start, in units per second (from an interrupted animation)
    v0: f32,
}

impl Anim {
    /// At `value`, not moving.
    pub fn at(value: f32) -> Anim {
        Anim { from: value, to: value, start: Instant::now(), duration: Duration::ZERO, curve: Curve::Linear, v0: 0.0 }
    }

    /// From `from` to `to` over `duration` along `curve` (a spring takes the time it needs).
    pub fn new(from: f32, to: f32, duration: Duration, curve: Curve) -> Anim {
        let curve = calm(curve);
        Anim { from, to, start: Instant::now(), duration: effective(duration, curve), curve, v0: 0.0 }
    }

    fn progress(&self, now: Instant) -> (f32, f32) {
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        let span = self.to - self.from;
        let v0n = if span.abs() > 1e-6 { self.v0 / span } else { 0.0 };
        match self.curve {
            Curve::Spring { response, damping } => spring(response, damping, v0n, t),
            c => {
                let d = self.duration.as_secs_f32();
                if d <= 0.0 {
                    return (1.0, 0.0);
                }
                let x = (t / d).min(1.0);
                let f = |x: f32| match c {
                    Curve::Linear => x,
                    Curve::Standard => cubic_bezier(0.42, 0.0, 0.58, 1.0, x),
                    Curve::Decelerate => cubic_bezier(0.0, 0.0, 0.2, 1.0, x),
                    Curve::Accelerate => cubic_bezier(0.4, 0.0, 1.0, 1.0, x),
                    Curve::Spring { .. } => unreachable!(),
                };
                let y = f(x);
                let dy = if x < 1.0 { (f((x + 0.01).min(1.0)) - y) / 0.01 / d } else { 0.0 };
                (y, dy)
            }
        }
    }

    pub fn value_at(&self, now: Instant) -> f32 {
        let (p, _) = self.progress(now);
        self.from + (self.to - self.from) * p
    }

    pub fn value(&self) -> f32 {
        self.value_at(Instant::now())
    }

    /// Units per second now.
    pub fn velocity_at(&self, now: Instant) -> f32 {
        let (_, dp) = self.progress(now);
        (self.to - self.from) * dp
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    pub fn done_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= self.duration
    }

    pub fn done(&self) -> bool {
        self.done_at(Instant::now())
    }

    /// Go to `to` from wherever the value is now, keeping its speed (springs) — an interrupted
    /// animation turns around smoothly instead of jumping.
    pub fn retarget(&mut self, to: f32, duration: Duration, curve: Curve) {
        let now = Instant::now();
        self.retarget_at(to, duration, curve, now);
    }

    pub fn retarget_at(&mut self, to: f32, duration: Duration, curve: Curve, now: Instant) {
        if (to - self.to).abs() < 1e-6 && !self.done_at(now) {
            return;
        }
        let curve = calm(curve);
        let (v, vel) = (self.value_at(now), self.velocity_at(now));
        *self = Anim { from: v, to, start: now, duration: effective(duration, curve), curve, v0: if matches!(curve, Curve::Spring { .. }) { vel } else { 0.0 } };
    }
}

/// With reduced motion a spring is a short eased step (no bounce).
fn calm(curve: Curve) -> Curve {
    if reduced() && matches!(curve, Curve::Spring { .. }) {
        Curve::Decelerate
    } else {
        curve
    }
}

/// A spring lasts until it settles; reduced motion makes everything (nearly) instant.
fn effective(d: Duration, curve: Curve) -> Duration {
    if reduced() {
        return Duration::from_millis(d.as_millis().min(90) as u64);
    }
    match curve {
        Curve::Spring { response, damping } => Duration::from_secs_f32(spring_settles(response, damping)),
        _ => d,
    }
}

static REDUCED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

/// Movement is replaced by quick fades: Windows' "Animation effects" off (setting 0), or the
/// user chose reduced (1), not full (2).
pub fn reduced() -> bool {
    match REDUCED.load(std::sync::atomic::Ordering::Relaxed) {
        1 => true,
        0 => !system_animations(),
        _ => false,
    }
}

/// Settings > Animations: 0 as Windows says, 1 reduced, 2 full.
pub fn set_preference(p: u8) {
    REDUCED.store(p.min(2), std::sync::atomic::Ordering::Relaxed);
}

#[cfg(windows)]
fn system_animations() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut on = windows::core::BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    on.as_bool()
}

#[cfg(not(windows))]
fn system_animations() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_stay_in_their_ranges() {
        assert_eq!(tokens::pick(tokens::PRESS, 0.0), Duration::from_millis(80));
        assert_eq!(tokens::pick(tokens::WINDOW, 1.0), Duration::from_millis(360));
        assert_eq!(tokens::pick(tokens::MENU, 0.5), Duration::from_millis(200));
        assert_eq!(tokens::pick(tokens::MODE, 7.0), Duration::from_millis(420));
    }

    #[test]
    fn bezier_curves() {
        for x in [0.0, 1.0] {
            assert!((cubic_bezier(0.42, 0.0, 0.58, 1.0, x) - x).abs() < 1e-3);
        }
        assert!((cubic_bezier(0.42, 0.0, 0.58, 1.0, 0.5) - 0.5).abs() < 1e-3, "symmetric");
        assert!((cubic_bezier(0.0, 0.0, 0.2, 1.0, 0.25) - 0.5776).abs() < 2e-3, "decelerate starts fast");
        assert!((cubic_bezier(0.4, 0.0, 1.0, 1.0, 0.25) - 0.0986).abs() < 2e-3, "accelerate starts slow");
        let mut last = 0.0;
        for i in 0..=100 {
            let y = cubic_bezier(0.42, 0.0, 0.58, 1.0, i as f32 / 100.0);
            assert!(y >= last - 1e-4, "monotonic");
            last = y;
        }
    }

    #[test]
    fn springs_settle_and_overshoot_only_when_underdamped() {
        let (x, _) = spring(0.3, 1.0, 0.0, 2.0);
        assert!((x - 1.0).abs() < 1e-3);
        let peak = (0..400).map(|i| spring(0.3, 1.0, 0.0, i as f32 / 200.0).0).fold(0.0f32, f32::max);
        assert!(peak <= 1.0005, "critically damped: no overshoot ({peak})");
        let peak = (0..400).map(|i| spring(0.3, 0.6, 0.0, i as f32 / 200.0).0).fold(0.0f32, f32::max);
        assert!(peak > 1.05, "underdamped: overshoots ({peak})");
        let s = spring_settles(0.32, 0.86);
        assert!(s > 0.2 && s < 1.0, "{s}");
    }

    #[test]
    fn an_interrupted_animation_turns_around_without_a_jump() {
        let t0 = Instant::now();
        let mut a = Anim { from: 0.0, to: 100.0, start: t0, duration: Duration::from_secs_f32(spring_settles(0.3, 1.0)), curve: Curve::Spring { response: 0.3, damping: 1.0 }, v0: 0.0 };
        let mid = t0 + Duration::from_millis(80);
        let (v, speed) = (a.value_at(mid), a.velocity_at(mid));
        assert!(v > 5.0 && v < 95.0 && speed > 0.0, "{v} {speed}");
        a.retarget_at(0.0, Duration::from_millis(300), Curve::Spring { response: 0.3, damping: 1.0 }, mid);
        assert!((a.value_at(mid) - v).abs() < 0.01, "no jump at the turn");
        // it keeps going up for a moment (its speed), then comes back
        assert!(a.value_at(mid + Duration::from_millis(10)) > v);
        assert!(a.value_at(mid + Duration::from_secs(2)).abs() < 0.2);
    }

    #[test]
    fn eased_animations_end_on_time() {
        let t0 = Instant::now();
        let a = Anim { from: 10.0, to: 20.0, start: t0, duration: Duration::from_millis(200), curve: Curve::Standard, v0: 0.0 };
        assert_eq!(a.value_at(t0), 10.0);
        assert!((a.value_at(t0 + Duration::from_millis(100)) - 15.0).abs() < 0.1);
        assert_eq!(a.value_at(t0 + Duration::from_millis(250)), 20.0);
        assert!(a.done_at(t0 + Duration::from_millis(200)) && !a.done_at(t0 + Duration::from_millis(150)));
    }

    #[test]
    fn reduced_motion_shortens_everything() {
        set_preference(1);
        assert!(reduced());
        let a = Anim::new(0.0, 1.0, Duration::from_millis(360), Curve::ARRIVE);
        assert!(a.duration <= Duration::from_millis(90));
        set_preference(2);
        assert!(!reduced());
    }
}
