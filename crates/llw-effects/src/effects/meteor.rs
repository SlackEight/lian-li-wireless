//! Meteor effect — a bright head trails an exponential fade across the chain.
//!
//! # Algorithm
//!
//! ```text
//! axis  = chain_pos(fan, i, fan_count, leds_per_fan)   -- for Fans
//!       = strip_pos(i, total)                            -- for Strip
//! h     = t                                              -- head position ∈ [0,1)
//! d     = (h - pos).rem_euclid(1.0)                     -- distance behind head
//! color = scale(palette(pos), exp(-d * 12.0))
//! ```
//!
//! `d` is computed with `rem_euclid` so values wrap: a position just *ahead* of
//! the head gets `d ≈ 1.0` (essentially black), while a position just *behind*
//! gets `d ≈ 0` (bright).  This produces a one-sided trailing glow — the head
//! is at full brightness and the tail decays exponentially behind it.
//!
//! # Decay constants
//!
//! The scale factor `exp(-d * 12)` gives:
//!
//! | d    | factor  | 255 × factor |
//! |------|---------|--------------|
//! | 0.00 | 1.000   | 255          |
//! | 0.05 | 0.549   | 140          |
//! | 0.10 | 0.301   |  77          |
//! | 0.20 | 0.091   |  23          |
//! | 0.35 | 0.015   |   3.8 → ≤4  |
//! | 1.00 | ≈6.1e-6 |   ≈0         |
//!
//! Beyond `d ≈ 0.35` every channel rounds to ≤ 4 (effectively black).

use crate::{color, EffectSpec};
use crate::geometry::{self, Geometry};

/// Render one frame of the **Meteor** effect at phase `t ∈ [0, 1)`.
///
/// Head is at chain/strip position `h = t`.  Each LED's brightness is
/// `exp(−d × 12)` where `d = (h − pos).rem_euclid(1.0)` is the trailing
/// distance from the head (0 at the head, approaching 1 just ahead of it).
///
/// Color at each LED = `scale(palette(pos), brightness_factor)`.
pub fn render(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    match geom {
        Geometry::Fans { fan_count, leds_per_fan, .. } => {
            let fc = *fan_count;
            let lf = *leds_per_fan;
            let mut frame = Vec::with_capacity(fc as usize * lf as usize);
            for fan in 0..fc {
                for i in 0..lf {
                    let pos = geometry::chain_pos(fan, i, fc, lf);
                    let d = (t - pos).rem_euclid(1.0);
                    let brightness = (-d * 12.0_f32).exp();
                    let base = color::palette(&spec.colors, pos);
                    frame.push(color::scale(base, brightness));
                }
            }
            frame
        }
        Geometry::Strip { total } => {
            let n = *total;
            (0..n)
                .map(|i| {
                    let pos = geometry::strip_pos(i, n);
                    let d = (t - pos).rem_euclid(1.0);
                    let brightness = (-d * 12.0_f32).exp();
                    let base = color::palette(&spec.colors, pos);
                    color::scale(base, brightness)
                })
                .collect()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectKind, Direction, EffectSpec};

    fn spec() -> EffectSpec {
        EffectSpec {
            kind: EffectKind::Meteor,
            colors: vec![],   // default white palette
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    fn fans() -> Geometry { Geometry::Fans { fan_count: 3, leds_per_fan: 44, layout: crate::geometry::FanLayout::UniformRing } }
    fn strip() -> Geometry { Geometry::Strip { total: 132 } }

    fn brightness_sum(c: [u8; 3]) -> u32 {
        c[0] as u32 + c[1] as u32 + c[2] as u32
    }

    // ---- head LED is brightest ----
    //
    // At t=0.0 the head is at pos=0 (chain_pos=0, strip_pos=0).
    // d = (0.0 - 0.0).rem_euclid(1.0) = 0.0 → exp(0) = 1.0 → [255, 255, 255]
    // All other LEDs have pos > 0, so d = (0 - pos).rem_euclid(1.0) = 1 - pos
    // which is close to 1.0 → exp(-12 * ~1) ≈ 6e-6 → near black.
    // Exception: LED at pos just below 1.0 wraps to d ≈ 0, but strip_pos(0)=0
    // is the global minimum, so LED 0 has d=0 and is uniquely brightest.

    #[test]
    fn head_led_brightest_strip() {
        let frame = render(&spec(), &strip(), 0.0);
        let head = frame[0];
        // All other LEDs must be strictly dimmer than the head
        for (i, &led) in frame.iter().enumerate().skip(1) {
            assert!(
                brightness_sum(led) < brightness_sum(head),
                "LED {i} (brightness_sum={}) must be dimmer than head ({})",
                brightness_sum(led),
                brightness_sum(head)
            );
        }
    }

    #[test]
    fn head_led_brightest_fans() {
        let frame = render(&spec(), &fans(), 0.0);
        let head = frame[0];
        for (i, &led) in frame.iter().enumerate().skip(1) {
            assert!(
                brightness_sum(led) < brightness_sum(head),
                "LED {i} (brightness_sum={}) must be dimmer than head ({})",
                brightness_sum(led),
                brightness_sum(head)
            );
        }
    }

    // ---- strictly decreasing brightness along 10 trailing LEDs ----
    //
    // For Strip{132} at t=0.5, head is at pos=0.5 (LED 66).
    // LEDs 65, 64, 63, ... trail behind: their pos is slightly less than 0.5,
    // so d = (0.5 - pos).rem_euclid(1.0) = 0.5 - pos grows as pos decreases.
    // Higher d → lower brightness → brightness_sum strictly decreases along trail.
    //
    // Trail: LED 66 (head, d=0), LED 65 (d=1/132), LED 64 (d=2/132), ...
    // d_k = k/132 for LED (66-k), so brightness_k = exp(-12 * k/132).
    // Strictly decreasing since exp is strictly decreasing.

    #[test]
    fn tail_strictly_decreasing_brightness_strip() {
        let frame = render(&spec(), &strip(), 0.5);
        // Head at LED 66 (pos=66/132=0.5 exactly)
        // Trailing: 65, 64, 63, ..., 57 (10 LEDs behind)
        let head_idx = 66usize;
        let mut prev_sum = brightness_sum(frame[head_idx]);
        for k in 1..=10 {
            let idx = head_idx - k;
            let curr_sum = brightness_sum(frame[idx]);
            assert!(
                curr_sum < prev_sum,
                "LED {idx} (sum={curr_sum}) must be dimmer than LED {} (sum={prev_sum})",
                idx + 1
            );
            prev_sum = curr_sum;
        }
    }

    // ---- beyond d > 0.35, LED effectively black (each channel ≤ 4) ----
    //
    // exp(-0.35 * 12) = exp(-4.2) ≈ 0.01500 → 255 * 0.015 = 3.825 → rounds to 4.
    // For default white palette [255,255,255], scale([255,255,255], exp(-d*12))
    // gives each channel = (255 * exp(-d*12)).round().
    // At d=0.35: ≤ 4. At d > 0.35: even smaller.
    //
    // For Strip{132} at t=0.0, head at LED 0 (pos=0).
    // LED i has pos=i/132; d=(0-i/132).rem_euclid(1)=1-i/132 for i>0.
    // We want d < 0.35, i.e. 1-i/132 < 0.35 → i/132 > 0.65 → i > 85.8 → i≥86.
    // Wait — that means LEDs 86..131 have d ∈ (0.35, 1), which grows away from head.
    // But the TAIL is behind the head: LEDs just BEFORE wrapping (i=131, d=1/132≈0.0076).
    // LEDs ahead of the head (e.g. i=50, pos=50/132≈0.379, d=(0-0.379).rem_euclid(1)=0.621) → black.
    // So for i in 1..86 (ahead of head): d=1-i/132 ∈ (0.35, ~1) → effectively black.
    // Spot-check LED 50: d = (0.0 - 50.0/132.0).rem_euclid(1.0) = 1.0 - 50.0/132.0 ≈ 0.621
    // exp(-0.621 * 12) = exp(-7.45) ≈ 0.000583 → 255 * 0.000583 ≈ 0.15 → rounds to 0.

    #[test]
    fn far_ahead_leds_effectively_black() {
        let frame = render(&spec(), &strip(), 0.0);
        // LEDs 10..85 are far ahead of the head (d ∈ [0.35, 0.92]): must be nearly black
        // (each channel ≤ 4, matching exp(-0.35*12)*255 = exp(-4.2)*255 ≈ 3.8 boundary)
        for (i, &c) in frame.iter().enumerate().take(86).skip(10) {
            assert!(
                c[0] <= 4 && c[1] <= 4 && c[2] <= 4,
                "LED {i} (d>0.35) should be effectively black, got {:?}", c
            );
        }
    }

    // ---- direction: Reverse Meteor head at frame 6 of 24 == Forward at t=0.75 ----
    //
    // render_animation with Reverse on a directional kind applies t → 1-t.
    // Forward frame 6 of 24: t = 6/24 = 0.25
    // Reverse frame 6 of 24: t_effective = 1 - 6/24 = 0.75
    // Forward frame 18 of 24: t = 18/24 = 0.75
    // So Reverse[6] must equal Forward[18].

    #[test]
    fn meteor_direction_reverse_maps_t() {
        use crate::{render_animation, Direction};
        let geom = strip();
        let frames = 24u16;

        let spec_fwd = EffectSpec {
            kind: EffectKind::Meteor,
            colors: vec![],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        };
        let (fwd, _) = render_animation(&spec_fwd, &geom, frames);

        let spec_rev = EffectSpec {
            kind: EffectKind::Meteor,
            colors: vec![],
            speed: 3,
            direction: Direction::Reverse,
            brightness: 4,
        };
        let (rev, _) = render_animation(&spec_rev, &geom, frames);

        // Reverse frame 6 rendered at t = 1 - 6/24 = 0.75
        // Forward frame 18 rendered at t = 18/24 = 0.75
        assert_eq!(
            rev[6], fwd[18],
            "Reverse frame 6 (t=0.75) must equal Forward frame 18 (t=0.75)"
        );
    }
}
