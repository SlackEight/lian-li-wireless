//! Rainbow and RainbowMorph effects.
//!
//! This module hosts both effects because they share the hue-wheel concept:
//!
//! - **RainbowMorph** (Task 2, uniform): `hue = t` — the whole device shifts
//!   through the hue wheel together, one full rotation per period.
//! - **Rainbow** (Task 3, positional): `hue = (axis + t) mod 1` — each LED's
//!   hue is offset by its ring angle / strip position, producing a moving band.
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
use crate::geometry::Geometry;

/// Render one frame of the **RainbowMorph** effect at phase `t ∈ [0, 1)`.
///
/// Every LED receives `hsv_to_rgb(t, 1.0, 1.0)` — a single hue that advances
/// uniformly across the hue wheel over one period.
pub fn render_morph(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    let _ = spec; // palette / speed used by caller; no per-LED parameters here
    let led = color::hsv_to_rgb(t, 1.0, 1.0);
    vec![led; geom.len()]
}

// Task 3: Rainbow (positional) — see render_rainbow stub below.
//
// pub fn render_rainbow(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
//     todo!("Task 3: Rainbow — hue = (axis + t).rem_euclid(1.0), full S/V")
// }

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

    fn fans() -> Geometry { Geometry::Fans { fan_count: 3, leds_per_fan: 44 } }
    fn strip() -> Geometry { Geometry::Strip { total: 132 } }

    // ---- uniformity ----

    #[test]
    fn all_leds_equal_fans() {
        let frame = render_morph(&spec_morph(), &fans(), 0.33);
        let first = frame[0];
        assert!(frame.iter().all(|&c| c == first), "all LEDs must be equal (fans)");
    }

    #[test]
    fn all_leds_equal_strip() {
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
}
