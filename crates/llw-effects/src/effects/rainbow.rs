//! Rainbow and RainbowMorph effects.
//!
//! This module hosts both effects because they share the hue-wheel concept:
//!
//! - **RainbowMorph** (Task 2, uniform): `hue = t` — the whole device shifts
//!   through the hue wheel together, one full rotation per period.
//! - **Rainbow** (Task 3, positional): `hue = (axis + t) mod 1` — each LED's
//!   hue is offset by its ring angle (Fans) or strip position (Strip), producing
//!   a moving colour band that scrolls across the device.
//!
//! # Rainbow algorithm
//!
//! ```text
//! axis  = ring_angle(i, leds_per_fan)          -- for Fans
//!       = strip_pos(i, total)                   -- for Strip
//! hue   = (axis + t).rem_euclid(1.0)
//! color = hsv_to_rgb(hue, 1.0, 1.0)
//! ```
//!
//! Because the axis for Fans is the **ring angle** (not chain position), every
//! fan shows the full hue wheel independently — LED 0 of fan 0, fan 1, and fan 2
//! always have the same hue at any phase `t`.
//!
//! # RainbowMorph algorithm
//!
//! ```text
//! LED color = hsv_to_rgb(t, 1.0, 1.0)
//! ```
//!
//! All LEDs are identical at each phase; no palette is used.

use crate::color;
use crate::EffectSpec;
use crate::geometry::{self, Geometry};

/// Render one frame of the **Rainbow** effect at phase `t ∈ [0, 1)`.
///
/// - **Fans**: axis = ring angle (`i / leds_per_fan`). Every fan displays the
///   full hue wheel; the wheel scrolls forward as `t` increases.
/// - **Strip**: axis = strip position (`i / total`). The strip displays the
///   full hue wheel linearly; it scrolls forward as `t` increases.
///
/// No palette is used — full saturation and value always.
pub fn render_rainbow(_spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    match geom {
        Geometry::Fans { fan_count, leds_per_fan, layout } => {
            let fc = *fan_count;
            let lf = *leds_per_fan;
            let mut frame = Vec::with_capacity(fc as usize * lf as usize);
            for _fan in 0..fc {
                for i in 0..lf {
                    let axis = geometry::led_polar(*layout, i, lf).0;
                    let hue = (axis + t).rem_euclid(1.0);
                    frame.push(color::hsv_to_rgb(hue, 1.0, 1.0));
                }
            }
            frame
        }
        Geometry::Strip { total } => {
            let n = *total;
            (0..n)
                .map(|i| {
                    let axis = geometry::strip_pos(i, n);
                    let hue = (axis + t).rem_euclid(1.0);
                    color::hsv_to_rgb(hue, 1.0, 1.0)
                })
                .collect()
        }
    }
}

/// Render one frame of the **RainbowMorph** effect at phase `t ∈ [0, 1)`.
///
/// Every LED receives `hsv_to_rgb(t, 1.0, 1.0)` — a single hue that advances
/// uniformly across the hue wheel over one period.
pub fn render_morph(_spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    let led = color::hsv_to_rgb(t, 1.0, 1.0);
    vec![led; geom.len()]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectKind, Direction};

    fn spec_morph() -> EffectSpec {
        EffectSpec {
            kind: EffectKind::RainbowMorph,
            colors: vec![],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    fn spec_rainbow() -> EffectSpec {
        EffectSpec {
            kind: EffectKind::Rainbow,
            colors: vec![],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    fn fans() -> Geometry { Geometry::Fans { fan_count: 3, leds_per_fan: 44, layout: crate::geometry::FanLayout::UniformRing } }
    fn strip() -> Geometry { Geometry::Strip { total: 132 } }

    // =========================================================================
    // RainbowMorph tests (from Task 2 — preserved)
    // =========================================================================

    // ---- uniformity ----

    #[test]
    fn morph_all_leds_equal_fans() {
        let frame = render_morph(&spec_morph(), &fans(), 0.33);
        let first = frame[0];
        assert!(frame.iter().all(|&c| c == first), "all LEDs must be equal (fans)");
    }

    #[test]
    fn morph_all_leds_equal_strip() {
        let frame = render_morph(&spec_morph(), &strip(), 0.33);
        let first = frame[0];
        assert!(frame.iter().all(|&c| c == first), "all LEDs must be equal (strip)");
    }

    // ---- golden values ----
    //
    // hsv_to_rgb(t, 1.0, 1.0) uses the standard sextant algorithm:
    //   h_scaled = t * 6;  i = floor(h_scaled);  f = h_scaled - i
    //   p = 0  (since s=1, v=1: v*(1-s) = 0)
    //   q = 1 - f   (v*(1-s*f) = 1-f)
    //   t_local = f (v*(1-s*(1-f)) = f)
    //
    // t = 0.00: h_scaled=0.0, i=0, f=0 → sextant 0: (v,t_local,p)=(1,0,0) → [255,  0,  0]  red
    // t = 0.25: h_scaled=1.5, i=1, f=0.5
    //           q=1-0.5=0.5  t_local=0.5  → sextant 1: (q,v,p)=(0.5,1,0)
    //           → [(0.5*255).round(),(1*255).round(),0] = [128,255,0]  chartreuse
    // t = 0.50: h_scaled=3.0, i=3, f=0
    //           q=1-0=1  t_local=0  → sextant 3: (p,q,v)=(0,1,1)
    //           → [0,255,255]  cyan

    #[test]
    fn golden_morph_fans_t0() {
        let frame = render_morph(&spec_morph(), &fans(), 0.0);
        // h=0 → pure red
        assert_eq!(frame[0],   [255, 0, 0], "LED 0   t=0 fans");
        assert_eq!(frame[65],  [255, 0, 0], "LED 65  t=0 fans");
        assert_eq!(frame[131], [255, 0, 0], "LED 131 t=0 fans");
    }

    #[test]
    fn golden_morph_fans_t025() {
        // h=0.25: sextant 1, f=0.5 → (q,v,p)=(0.5,1,0) → [128,255,0]
        let frame = render_morph(&spec_morph(), &fans(), 0.25);
        assert_eq!(frame[0],   [128, 255, 0], "LED 0   t=0.25 fans");
        assert_eq!(frame[65],  [128, 255, 0], "LED 65  t=0.25 fans");
        assert_eq!(frame[131], [128, 255, 0], "LED 131 t=0.25 fans");
    }

    #[test]
    fn golden_morph_fans_t05() {
        // h=0.5: sextant 3, f=0 → (p,q,v)=(0,1,1) → [0,255,255]
        let frame = render_morph(&spec_morph(), &fans(), 0.5);
        assert_eq!(frame[0],   [0, 255, 255], "LED 0   t=0.5 fans");
        assert_eq!(frame[65],  [0, 255, 255], "LED 65  t=0.5 fans");
        assert_eq!(frame[131], [0, 255, 255], "LED 131 t=0.5 fans");
    }

    #[test]
    fn golden_morph_strip_t0() {
        let frame = render_morph(&spec_morph(), &strip(), 0.0);
        assert_eq!(frame[0],   [255, 0, 0], "LED 0   t=0 strip");
        assert_eq!(frame[65],  [255, 0, 0], "LED 65  t=0 strip");
        assert_eq!(frame[131], [255, 0, 0], "LED 131 t=0 strip");
    }

    #[test]
    fn golden_morph_strip_t025() {
        let frame = render_morph(&spec_morph(), &strip(), 0.25);
        assert_eq!(frame[0],   [128, 255, 0], "LED 0   t=0.25 strip");
        assert_eq!(frame[65],  [128, 255, 0], "LED 65  t=0.25 strip");
        assert_eq!(frame[131], [128, 255, 0], "LED 131 t=0.25 strip");
    }

    #[test]
    fn golden_morph_strip_t05() {
        let frame = render_morph(&spec_morph(), &strip(), 0.5);
        assert_eq!(frame[0],   [0, 255, 255], "LED 0   t=0.5 strip");
        assert_eq!(frame[65],  [0, 255, 255], "LED 65  t=0.5 strip");
        assert_eq!(frame[131], [0, 255, 255], "LED 131 t=0.5 strip");
    }

    // ---- hue advances through full wheel over one period ----

    #[test]
    fn morph_hue_advances() {
        let g = fans();
        let s = spec_morph();
        // At t=0 red (high R, low G/B), at t=1/3 green (high G), at t=2/3 blue (high B)
        let f0 = render_morph(&s, &g, 0.0);
        let f1 = render_morph(&s, &g, 1.0 / 3.0);
        let f2 = render_morph(&s, &g, 2.0 / 3.0);
        // t=0 → red: R highest
        assert!(f0[0][0] > f0[0][1] && f0[0][0] > f0[0][2]);
        // t=1/3 → green: G highest
        assert!(f1[0][1] > f1[0][0] && f1[0][1] > f1[0][2]);
        // t=2/3 → blue: B highest
        assert!(f2[0][2] > f2[0][0] && f2[0][2] > f2[0][1]);
    }

    // =========================================================================
    // Rainbow (positional) tests — Task 3
    // =========================================================================

    // ---- golden: LED at axis=0, t=0 → hue 0 = red ----
    //
    // Strip{132}: LED 0 has axis = strip_pos(0,132) = 0/132 = 0.0
    // hue = (0.0 + 0.0).rem_euclid(1.0) = 0.0
    // hsv_to_rgb(0.0, 1.0, 1.0) → sextant 0, f=0 → [255, 0, 0]

    #[test]
    fn rainbow_strip_led0_t0_is_red() {
        let frame = render_rainbow(&spec_rainbow(), &strip(), 0.0);
        // LED 0: axis=0, t=0 → hue=0 → red
        assert_eq!(frame[0], [255, 0, 0], "LED 0 at axis=0, t=0 must be hue 0 (red)");
    }

    // ---- golden: Fans LED at axis=0, t=0 → hue 0 = red ----
    //
    // Fans{3,44}: LED 0 of fan 0 has ring_angle(0,44) = 0.0
    // hue = (0.0 + 0.0).rem_euclid(1.0) = 0.0 → [255, 0, 0]

    #[test]
    fn rainbow_fans_led0_t0_is_red() {
        let frame = render_rainbow(&spec_rainbow(), &fans(), 0.0);
        assert_eq!(frame[0], [255, 0, 0], "LED 0 (axis=0), t=0 must be hue 0 (red)");
    }

    // ---- rotation property: frame at t=0.5 equals frame at t=0 with axis shifted 0.5 ----
    //
    // For Strip{132}:
    //   frame_t0[i]  has hue = (i/132 + 0.0).rem_euclid(1.0) = i/132
    //   frame_t05[i] has hue = (i/132 + 0.5).rem_euclid(1.0)
    //
    // At i=0, t=0.5: hue = (0 + 0.5) = 0.5
    //   hue=0.5: h_scaled=3.0, i=3, f=0 → sextant 3: (p,q,v)=(0,1,1) → [0,255,255] cyan
    //
    // At i=66, t=0: hue = 66/132 = 0.5 → same [0,255,255]
    //
    // Therefore frame_t05[0] == frame_t0[66].

    #[test]
    fn rainbow_strip_rotation_property() {
        let s = spec_rainbow();
        let g = strip();
        let frame_t0  = render_rainbow(&s, &g, 0.0);
        let frame_t05 = render_rainbow(&s, &g, 0.5);
        // LED 0 at t=0.5: hue = (0/132 + 0.5) = 0.5 → [0,255,255] cyan
        // LED 66 at t=0:  hue = 66/132 = 0.5       → [0,255,255] cyan
        assert_eq!(
            frame_t05[0], frame_t0[66],
            "frame_t05[0] (axis=0,t=0.5) must equal frame_t0[66] (axis=0.5,t=0)"
        );
        // Independent golden: both must be cyan (hue=0.5 → [0,255,255])
        assert_eq!(frame_t05[0], [0, 255, 255], "hue=0.5 must be cyan");
        assert_eq!(frame_t0[66], [0, 255, 255], "LED 66 at t=0 must be cyan");
    }

    // ---- per-fan wheel: LED 0 of each fan has the same hue at any t ----
    //
    // All fans have ring_angle(0, 44) = 0.0, so their first LED always gets
    // hue = (0.0 + t).rem_euclid(1.0) — identical across fans.

    #[test]
    fn rainbow_fans_per_fan_wheel() {
        let s = spec_rainbow();
        let g = fans(); // Fans{3, 44}
        let leds_per_fan = 44usize;

        for &t in &[0.0_f32, 0.25, 0.5, 0.75] {
            let frame = render_rainbow(&s, &g, t);
            let fan0_led0 = frame[0];
            let fan1_led0 = frame[leds_per_fan];
            let fan2_led0 = frame[2 * leds_per_fan];
            assert_eq!(
                fan0_led0, fan1_led0,
                "LED 0 of fan 0 and fan 1 must match at t={t}"
            );
            assert_eq!(
                fan0_led0, fan2_led0,
                "LED 0 of fan 0 and fan 2 must match at t={t}"
            );
        }
    }

    // =========================================================================
    // Rainbow on SlInf44 — tests added in commit 2 (2026-07-14)
    // =========================================================================

    fn sl_inf44_fans() -> Geometry {
        Geometry::Fans {
            fan_count: 1,
            leds_per_fan: 44,
            layout: crate::geometry::FanLayout::SlInf44,
        }
    }

    // ---- left-arc / right-arc at same height are reflections ----
    //
    // idx 12 (outer LEFT arc, k=4):  angle = 0.5 + 4.5×0.05 = 0.725
    // idx 30 (outer RIGHT arc, k=4): angle = (0.5 − 4.5×0.05).rem_euclid(1) = 0.275
    //
    // These are left-right reflections: 0.725 + 0.275 = 1.0.
    // The uniform-ring assumption produced the SAME angle for both (all at radius 1.0
    // with ring_angle), causing the rainbow to mirror on the left arc.
    // With SlInf44, each LED gets its measured angle, and the rainbow is correct.
    //
    // At t=0:
    //   hue(idx 12) = (0.725 + 0.0).rem_euclid(1.0) = 0.725
    //   hue(idx 30) = (0.275 + 0.0).rem_euclid(1.0) = 0.275
    //
    // hsv_to_rgb(0.725):
    //   h_scaled = 0.725*6 = 4.35; i=4, f=0.35
    //   sextant 4: (p,q,v)=(0,v,q)=(0,1,0.65) wait — sextant 4 is (t_local,p,v):
    //   sextant 4: R=f, G=0, B=1 → R=(0.35*255).round()=89, G=0, B=255 → [89,0,255]
    //   Actually: sextant 4 formula: (t_local, p, v) → (f, 0, 1)
    //   R = f*255 = 0.35*255 = 89.25 → round → 89; G = 0; B = 255
    //
    // hsv_to_rgb(0.275):
    //   h_scaled = 0.275*6 = 1.65; i=1, f=0.65
    //   sextant 1: (q,v,p)=(1-0.65, 1, 0) → (0.35, 1, 0)
    //   R=0.35*255=89.25→89; G=255; B=0 → [89,255,0]

    #[test]
    fn rainbow_slinf44_left_right_arc_same_height_are_reflections() {
        let geom = sl_inf44_fans();
        let s = spec_rainbow();
        let frame = render_rainbow(&s, &geom, 0.0);

        // idx 12 (left arc k=4): angle=0.725
        // idx 30 (right arc k=4): angle=0.275
        // Verify angles are correct reflections (sum to 1.0) by checking colors:
        // hue 0.725 → [89, 0, 255]; hue 0.275 → [89, 255, 0]
        let left_led  = frame[12]; // left arc k=4, angle 0.725
        let right_led = frame[30]; // right arc k=4, angle 0.275 (mirror)

        // Both should have R≈89 (same hue distance from a primary).
        assert_eq!(left_led[0], right_led[0],
            "left-arc and right-arc LEDs at same height must have matching R channel \
             (angles are reflections: left 0.725, right 0.275)");

        // Left: high B, low G. Right: high G, low B (reflections in hue wheel).
        assert!(left_led[2] > left_led[1],
            "left arc LED (hue≈0.725) must have B > G, got {left_led:?}");
        assert!(right_led[1] > right_led[2],
            "right arc LED (hue≈0.275) must have G > B, got {right_led:?}");

        // Golden-pin: verify exact computed colors.
        assert_eq!(left_led,  [89, 0, 255], "idx 12 (left arc k=4, hue=0.725) must be [89,0,255]");
        assert_eq!(right_led, [89, 255, 0], "idx 30 (right arc k=4, hue=0.275) must be [89,255,0]");
    }

    // ---- inner ring LEDs at t=0 all have angle in [0.75, 0.625∪... ) ----
    //
    // Inner ring (idx 0-7): angles go from 0.75 to 0.625 (wrapping through 1.0→0.0→...).
    // All have radius 0.4 (unused by rainbow, only angle matters).
    // Check that consecutive inner-ring LEDs have different hues (they span the wheel).

    #[test]
    fn rainbow_slinf44_inner_ring_has_varied_hues() {
        let geom = sl_inf44_fans();
        let s = spec_rainbow();
        let frame = render_rainbow(&s, &geom, 0.0);
        // inner ring: indices 0..8. Angles span 0.75 to 0.625 (wrapping through 1.0/0.0).
        // Not all the same → first and last must differ.
        let first = frame[0]; // angle 0.75
        let last  = frame[7]; // angle 0.625
        assert_ne!(first, last, "inner ring first and last LED must have different colors");
    }

    // ---- direction: Reverse frame(t) for Strip equals Forward frame(1-t) ----
    //
    // render_animation applies t→1-t for directional Reverse effects.
    // This test verifies the property at the render_frame level via render_animation.

    #[test]
    fn rainbow_direction_reverse_mirrors_time() {
        use crate::{render_animation, EffectKind, Direction, EffectSpec};
        let geom = Geometry::Strip { total: 132 };
        let frames = 24u16;

        // Forward at frame 6 → t = 6/24 = 0.25
        let spec_fwd = EffectSpec {
            kind: EffectKind::Rainbow,
            colors: vec![],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        };
        let (fwd_frames, _) = render_animation(&spec_fwd, &geom, frames);

        // Reverse at frame 6 → t mapped to 1 - 6/24 = 0.75
        let spec_rev = EffectSpec {
            kind: EffectKind::Rainbow,
            colors: vec![],
            speed: 3,
            direction: Direction::Reverse,
            brightness: 4,
        };
        let (rev_frames, _) = render_animation(&spec_rev, &geom, frames);

        // Reverse frame 6 (rendered at t=0.75) must equal Forward frame 18 (t=18/24=0.75)
        assert_eq!(
            rev_frames[6], fwd_frames[18],
            "Reverse frame 6 (t=0.75) must equal Forward frame 18 (t=0.75)"
        );
    }
}
