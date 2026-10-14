//! Coarse wheel acceleration for the document viewport.
//!
//! Some platforms report a clicky wheel as one line event per notch. Applied
//! verbatim each notch moves a fixed distance, so a fast spin feels slow. This
//! keeps a small constant-space state machine over arrival times and returns a
//! bounded multiplier: 1.0 for isolated notches, growing while notches arrive
//! quickly in the same direction.

use egui::Vec2;

/// Idle time after which a burst is forgotten, in seconds.
const RESET_GAP_SECS: f64 = 0.3;
/// Notch spacing at or below which input counts as a continuous spin, in seconds.
const FAST_GAP_SECS: f64 = 0.08;
/// Multiplier gained per fast consecutive notch.
const BOOST_PER_TICK: f32 = 0.4;
/// Upper bound for the multiplier. Reachable: 1.0 + 13 * 0.4 = 6.2, capped.
const MAX_BOOST: f32 = 6.0;
/// Ticks that affect the output.
const MAX_TICKS: u32 = 13;
/// Extra ticks retained beyond the ceiling. They absorb a few slow events
/// before the visible multiplier starts to fall.
const HYSTERESIS_TICKS: u32 = 4;
/// Maximum scroll applied from one frame of coarse input, in points per axis.
/// Each event is clamped first; this caps the sum.
pub(crate) const MAX_FRAME_POINTS: f32 = 8192.0;
/// Per-event clamp, in native units (lines or pages). Keeps one hostile event
/// bounded before points conversion.
pub(crate) const MAX_EVENT_UNITS: f32 = 10.0;

/// Arrival-rate state for line and page wheel events. Constant space: no
/// per-event allocation regardless of input rate.
#[derive(Debug, Default)]
pub(crate) struct WheelBoost {
    last: f64,
    burst: u32,
    dir: i8,
}

impl WheelBoost {
    /// Returns the multiplier for one bucket of motion carrying `motion`.
    ///
    /// `motion` is signed in native units; only its sign matters. Zero means
    /// no motion. Non-finite time returns 1.0 and leaves state unchanged;
    /// backwards time resets the burst and returns 1.0.
    pub(crate) fn multiplier(&mut self, now: f64, motion: f32) -> f32 {
        if !now.is_finite() || !motion.is_finite() || motion == 0.0 {
            return 1.0;
        }
        // Exact: signum of finite non-zero is always exactly -1.0 or 1.0.
        let dir = if motion > 0.0 { 1 } else { -1 };
        if !self.last.is_finite() {
            self.last = now;
            self.burst = 0;
            self.dir = dir;
            return 1.0;
        }
        let idle = now - self.last;
        if !idle.is_finite() || idle < 0.0 {
            self.last = now;
            self.burst = 0;
            self.dir = dir;
            return 1.0;
        }
        if idle > RESET_GAP_SECS || dir != self.dir {
            self.burst = 0;
        } else if idle <= FAST_GAP_SECS {
            self.burst = self.burst.saturating_add(1).min(MAX_TICKS + HYSTERESIS_TICKS);
        } else {
            // Same direction but unhurried. Decay so an old burst does not stick.
            self.burst = self.burst.saturating_sub(1);
        }
        self.last = now;
        self.dir = dir;
        let ticks = self.burst.min(MAX_TICKS);
        // Ticks are at most 13, exactly representable, so the result is finite.
        (1.0 + ticks as f32 * BOOST_PER_TICK).min(MAX_BOOST)
    }

    /// Drops burst state. Call after a programmatic scroll jump.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Caps one frame of accumulated coarse scroll, per axis. A non-finite axis
/// maps to zero; the other axis is preserved.
pub(crate) fn cap_frame(delta: Vec2) -> Vec2 {
    Vec2::new(
        if delta.x.is_finite() { delta.x.clamp(-MAX_FRAME_POINTS, MAX_FRAME_POINTS) } else { 0.0 },
        if delta.y.is_finite() { delta.y.clamp(-MAX_FRAME_POINTS, MAX_FRAME_POINTS) } else { 0.0 },
    )
}

/// One frame of coarse input, in points. Page units are ignored here: one
/// page is already viewport-sized, so it always takes the egui path at 1.0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameInput {
    pub line: Vec2,
}

/// Folds raw wheel events into the frame total. Pure function of its inputs:
/// no clock, no context. Point events are ignored here; trackpads take the
/// precise path. Page events are ignored here; they take the egui path.
/// Invalid scales contribute nothing.
pub(crate) fn accumulate(events: &[egui::Event], line_points: f32, single_paging: bool) -> FrameInput {
    let line_points = sanitize_scale(line_points);
    let mut out = FrameInput::default();
    for event in events {
        let egui::Event::MouseWheel { unit, delta, modifiers, .. } = event else { continue };
        if modifiers.command || modifiers.ctrl {
            continue; // Zooming, not scrolling.
        }
        if !delta.is_finite() {
            continue;
        }
        if !matches!(unit, egui::MouseWheelUnit::Line) {
            continue;
        }
        // Paging owns vertical motion in single-page view, so vertical input
        // is not accumulated for scroll.
        let paging_y = single_paging && !modifiers.shift;
        if modifiers.shift {
            // Shift scrolls sideways, matching egui's own wheel handling.
            let sum = delta.x + delta.y;
            if !sum.is_finite() || sum == 0.0 {
                continue;
            }
            out.line.x += sum.clamp(-MAX_EVENT_UNITS, MAX_EVENT_UNITS) * line_points;
        } else {
            out.line.x += delta.x.clamp(-MAX_EVENT_UNITS, MAX_EVENT_UNITS) * line_points;
            if !paging_y {
                out.line.y += delta.y.clamp(-MAX_EVENT_UNITS, MAX_EVENT_UNITS) * line_points;
            }
        }
    }
    // Keep the total finite no matter how many events arrived; the multiplier
    // and the frame cap bound the rest.
    if !out.line.is_finite() {
        return FrameInput::default();
    }
    out.line.x = out.line.x.clamp(-MAX_FRAME_POINTS, MAX_FRAME_POINTS);
    out.line.y = out.line.y.clamp(-MAX_FRAME_POINTS, MAX_FRAME_POINTS);
    out
}

/// Converts the frame total to the extra scroll delta beyond egui's share.
///
/// egui always applies the base (`line_points`) through its own smoothing;
/// this returns only the surplus for a fast spin, so single notches add
/// exactly zero and feel identical to every other scroll area. At most one
/// multiplier update per axis, so a batched frame cannot boost itself.
/// Output is frame-capped.
pub(crate) fn boost_extra(input: FrameInput, boost_x: &mut WheelBoost, boost_y: &mut WheelBoost, now: f64) -> Vec2 {
    let mut extra = Vec2::ZERO;
    if input.line.x != 0.0 && input.line.x.is_finite() {
        extra.x += input.line.x * (boost_x.multiplier(now, input.line.x) - 1.0);
    }
    if input.line.y != 0.0 && input.line.y.is_finite() {
        extra.y += input.line.y * (boost_y.multiplier(now, input.line.y) - 1.0);
    }
    cap_frame(extra)
}

/// A validated points-per-unit scale. Invalid input maps to zero, so callers
/// with a broken scale scroll nothing instead of infinitely.
fn sanitize_scale(points: f32) -> f32 {
    if points.is_finite() && points > 0.0 { points } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_slow_notches_stay_unboosted() {
        let mut b = WheelBoost::default();
        assert_eq!(b.multiplier(1.0, -1.0), 1.0);
        assert_eq!(b.multiplier(1.5, -1.0), 1.0);
        assert_eq!(b.multiplier(2.0, -1.0), 1.0);
    }

    #[test]
    fn fast_spin_accelerates_then_caps() {
        let mut b = WheelBoost::default();
        assert_eq!(b.multiplier(1.0, -1.0), 1.0);
        let mut last = 1.0;
        for (i, t) in (1..=20).map(|i| 1.0 + f64::from(i) * 0.02).enumerate() {
            last = b.multiplier(t, -1.0);
            if i < 2 {
                assert!(last >= 1.0, "early burst must not jump, got {last}");
            }
        }
        assert!(last > 2.0, "a sustained fast spin must accelerate, got {last}");
        assert!(last <= MAX_BOOST, "boost must be capped, got {last}");
        assert_eq!(last, MAX_BOOST, "a long spin should reach the ceiling");
        for i in 1..=20 {
            let m = b.multiplier(1.5 + f64::from(i) * 0.02, -1.0);
            assert!(m <= MAX_BOOST, "boost must stay capped, got {m}");
        }
    }

    #[test]
    fn reversal_and_idle_reset_the_burst() {
        let mut b = WheelBoost::default();
        b.multiplier(1.0, -1.0);
        for i in 1..=6 {
            b.multiplier(1.0 + f64::from(i) * 0.02, -1.0);
        }
        let boosted = b.multiplier(1.0 + 7.0 * 0.02, -1.0);
        assert!(boosted > 1.0, "precondition: burst is boosted, got {boosted}");
        assert_eq!(b.multiplier(1.0 + 8.0 * 0.02, 1.0), 1.0);
        for i in 9..=14 {
            b.multiplier(1.0 + f64::from(i) * 0.02, 1.0);
        }
        assert_eq!(b.multiplier(5.0, 1.0), 1.0);
    }

    #[test]
    fn invalid_input_never_boosts_or_corrupts() {
        let mut b = WheelBoost::default();
        for bad_now in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(b.multiplier(bad_now, -1.0), 1.0);
        }
        for bad_motion in [0.0, -0.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(b.multiplier(1.0, bad_motion), 1.0);
        }
        assert_eq!(b.multiplier(2.0, -1.0), 1.0);
        b.reset();
        assert_eq!(b.multiplier(3.0, -1.0), 1.0);
    }

    #[test]
    fn same_timestamp_events_count_as_a_burst() {
        let mut b = WheelBoost::default();
        assert_eq!(b.multiplier(1.0, -1.0), 1.0);
        for _ in 0..6 {
            b.multiplier(1.0, -1.0);
        }
        assert!(b.multiplier(1.0, -1.0) > 1.0);
    }

    #[test]
    fn fractional_notches_share_the_burst() {
        let mut b = WheelBoost::default();
        b.multiplier(1.0, -0.25);
        for i in 1..=8 {
            b.multiplier(1.0 + f64::from(i) * 0.02, -0.25);
        }
        assert!(b.multiplier(1.0 + 9.0 * 0.02, -0.25) > 1.0);
    }

    #[test]
    fn sleep_wake_resets_the_burst() {
        let mut b = WheelBoost::default();
        b.multiplier(1.0, -1.0);
        for i in 1..=6 {
            b.multiplier(1.0 + f64::from(i) * 0.02, -1.0);
        }
        assert_eq!(b.multiplier(3600.0, -1.0), 1.0);
    }

    #[test]
    fn backwards_time_resets_without_corrupting() {
        let mut b = WheelBoost::default();
        b.multiplier(1.0, -1.0);
        b.multiplier(1.02, -1.0);
        assert_eq!(b.multiplier(0.5, -1.0), 1.0);
        assert_eq!(b.multiplier(0.52, -1.0), 1.4);
    }

    #[test]
    fn unhurried_spin_decays() {
        let mut b = WheelBoost::default();
        b.multiplier(1.0, -1.0);
        for i in 1..=8 {
            b.multiplier(1.0 + f64::from(i) * 0.02, -1.0);
        }
        let peak = b.multiplier(1.0 + 9.0 * 0.02, -1.0);
        assert!(peak > 1.0);
        // Same direction but slower than the fast gap: each event sheds one tick.
        let mut prev = peak;
        for i in 10..=13 {
            let m = b.multiplier(1.0 + f64::from(i) * 0.15, -1.0);
            assert!(m <= prev, "decay must not grow: {m} > {prev}");
            prev = m;
        }
    }

    #[test]
    fn hysteresis_holds_the_ceiling_briefly() {
        let mut b = WheelBoost::default();
        let mut t = 1.0;
        b.multiplier(t, -1.0);
        for _ in 0..20 {
            t += 0.02;
            b.multiplier(t, -1.0);
        }
        assert_eq!(b.multiplier(t + 0.02, -1.0), MAX_BOOST);
        // A couple of unhurried events consume hysteresis before output falls.
        t += 0.15;
        assert_eq!(b.multiplier(t, -1.0), MAX_BOOST);
        t += 0.15;
        assert_eq!(b.multiplier(t, -1.0), MAX_BOOST);
    }

    #[test]
    fn frame_cap_bounds_each_axis() {
        use egui::vec2;
        assert_eq!(cap_frame(vec2(40.0, -40.0)), vec2(40.0, -40.0));
        assert_eq!(cap_frame(vec2(1e9, -1e9)), vec2(MAX_FRAME_POINTS, -MAX_FRAME_POINTS));
        assert_eq!(cap_frame(vec2(f32::NAN, 40.0)), vec2(0.0, 40.0));
        assert_eq!(cap_frame(vec2(40.0, f32::INFINITY)), vec2(40.0, 0.0));
    }

    fn wheel(unit: egui::MouseWheelUnit, dx: f32, dy: f32) -> egui::Event {
        egui::Event::MouseWheel { unit, delta: egui::vec2(dx, dy), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE }
    }

    fn shifted(unit: egui::MouseWheelUnit, dx: f32, dy: f32) -> egui::Event {
        egui::Event::MouseWheel { unit, delta: egui::vec2(dx, dy), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::SHIFT }
    }

    #[test]
    fn isolated_notch_adds_no_extra() {
        use egui::MouseWheelUnit::Line;
        // One notch at rest adds exactly zero on top of egui's share, at every
        // configured base.
        for base in [50.0, 100.0, 200.0, 40.0, 8.0] {
            let input = accumulate(&[wheel(Line, 0.0, -1.0)], base, false);
            assert_eq!(input.line, egui::vec2(0.0, -base));
            let mut bx = WheelBoost::default();
            let mut by = WheelBoost::default();
            assert_eq!(boost_extra(input, &mut bx, &mut by, 1.0), egui::Vec2::ZERO);
        }
    }

    #[test]
    fn fast_spin_adds_a_bounded_extra() {
        use egui::MouseWheelUnit::Line;
        // Twenty fast frames of one notch: the extra grows past zero but the
        // frame total (base plus extra) stays capped.
        let mut bx = WheelBoost::default();
        let mut by = WheelBoost::default();
        let mut peak = 0.0f32;
        for i in 0..20 {
            let input = accumulate(&[wheel(Line, 0.0, -1.0)], 100.0, false);
            let extra = boost_extra(input, &mut bx, &mut by, 1.0 + f64::from(i) * 0.02);
            assert!(extra.y <= 0.0, "extra follows the spin direction, got {extra:?}");
            peak = peak.min(extra.y);
        }
        assert!(peak < 0.0, "a sustained spin must add an extra");
        assert!(peak >= -MAX_FRAME_POINTS, "extra stays frame-capped, got {peak}");
    }

    #[test]
    fn page_and_point_events_accumulate_nothing() {
        use egui::{MouseWheelUnit::Line, MouseWheelUnit::Page, MouseWheelUnit::Point};
        // Page units always take the egui path; point units take the precise
        // path. Neither reaches the boost.
        let input = accumulate(&[wheel(Page, 0.0, -1.0), wheel(Point, 0.0, -30.0)], 100.0, false);
        assert_eq!(input, FrameInput::default());
        // Mixed with a line notch, only the line part accumulates.
        let input = accumulate(&[wheel(Line, 0.0, -1.0), wheel(Page, 0.0, -1.0)], 100.0, false);
        assert_eq!(input.line, egui::vec2(0.0, -100.0));
        let mut bx = WheelBoost::default();
        let mut by = WheelBoost::default();
        assert_eq!(boost_extra(input, &mut bx, &mut by, 1.0), egui::Vec2::ZERO);
    }

    #[test]
    fn single_paging_drops_vertical_keeps_horizontal() {
        use egui::MouseWheelUnit::Line;
        let input = accumulate(&[wheel(Line, 5.0, -1.0)], 100.0, true);
        assert_eq!(input.line, egui::vec2(500.0, 0.0));
    }

    #[test]
    fn shift_merges_into_horizontal() {
        use egui::MouseWheelUnit::Line;
        let input = accumulate(&[shifted(Line, 1.0, -2.0)], 100.0, false);
        assert_eq!(input.line, egui::vec2(-100.0, 0.0));
    }

    #[test]
    fn invalid_scales_and_events_contribute_nothing() {
        use egui::MouseWheelUnit::Line;
        for bad in [f32::NAN, 0.0, -10.0, f32::INFINITY] {
            let input = accumulate(&[wheel(Line, 0.0, -1.0)], bad, false);
            assert_eq!(input.line, egui::Vec2::ZERO, "line scale {bad}");
        }
        // Zoom input, trackpad units, and hostile deltas never accumulate.
        let zoom =
            egui::Event::MouseWheel { unit: Line, delta: egui::vec2(0.0, -1.0), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::COMMAND };
        let key = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let input = accumulate(&[zoom, key, wheel(Line, 0.0, f32::NAN)], 100.0, false);
        assert_eq!(input, FrameInput::default());
    }
}
