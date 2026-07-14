//! Runway effect — marching on/off segments.
//!
//! # Algorithm
//!
//! ```text
//! axis  = chain_pos(fan, i, fan_count, leds_per_fan)   -- for Fans
//!       = strip_pos(i, total)                            -- for Strip
//! on    = ((pos * 6 + t * 3) mod 1) < 0.5
//! color = if on { colors[0] or white } else { colors[1] or black }
//! ```
//!
//! The formula `(pos * 6 + t * 3) mod 1` creates six evenly-spaced segments
//! across the device; the `+ t * 3` term makes them march forward at 3× the
//! phase speed.  At any instant, exactly half the position range is "on" and
//! half is "off" (duty cycle ≈ 50%).
//!
//! # Color interpretation
//!
//! | `colors` len | on-color    | off-color   |
//! |-------------|-------------|-------------|
//! | 0           | white       | black       |
//! | 1           | `colors[0]` | black       |
//! | ≥ 2         | `colors[0]` | `colors[1]` |

use crate::EffectSpec;
use crate::geometry::{self, Geometry};

/// Render one frame of the **Runway** effect at phase `t ∈ [0, 1)`.
///
/// Marching on/off segments: LED is on if `((pos*6 + t*3) mod 1) < 0.5`.
/// On-color = `colors[0]` or white; off-color = `colors[1]` or black.
pub fn render(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    let on_color  = if spec.colors.is_empty() { [255u8, 255, 255] } else { spec.colors[0] };
    let off_color = if spec.colors.len() >= 2  { spec.colors[1]   } else { [0u8, 0, 0]   };

    match geom {
        Geometry::Fans { fan_count, leds_per_fan, .. } => {
            let fc = *fan_count;
            let lf = *leds_per_fan;
            let mut frame = Vec::with_capacity(fc as usize * lf as usize);
            for fan in 0..fc {
                for i in 0..lf {
                    let pos = geometry::chain_pos(fan, i, fc, lf);
                    let on = ((pos * 6.0 + t * 3.0).rem_euclid(1.0)) < 0.5;
                    frame.push(if on { on_color } else { off_color });
                }
            }
            frame
        }
        Geometry::Strip { total } => {
            let n = *total;
            (0..n)
                .map(|i| {
                    let pos = geometry::strip_pos(i, n);
                    let on = ((pos * 6.0 + t * 3.0).rem_euclid(1.0)) < 0.5;
                    if on { on_color } else { off_color }
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
            kind: EffectKind::Runway,
            colors: vec![],   // on=white, off=black
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    fn strip() -> Geometry { Geometry::Strip { total: 132 } }
    fn fans() -> Geometry { Geometry::Fans { fan_count: 3, leds_per_fan: 44, layout: crate::geometry::FanLayout::UniformRing } }

    // ---- duty cycle ≈ 50% ----
    //
    // For Strip{132} at t=0, the on-condition is:
    //   ((i/132) * 6 mod 1) < 0.5   ≡   (i/22) mod 1 < 0.5
    //
    // The pattern repeats every 22 LEDs (132/6 = 22 LEDs per segment):
    //   - LEDs 0..10  in each group: (i/22) ∈ [0, 10/22) → on  (11 LEDs)
    //   - LEDs 11..21 in each group: (i/22) ∈ [11/22, 1) → off (11 LEDs)
    //
    // 6 groups × 11 on = 66 on, 66 off.  The exact integer answer is 66.
    //
    // f32 precision note: the boundary is at i=11 where pos*6 = 11/22 = 0.5 exactly
    // in real arithmetic.  In f32, (11.0f32/132.0f32)*6.0 may land fractionally
    // above or below 0.5 due to rounding, shifting one boundary LED.  We allow
    // a tolerance of ±2 (range 64..=68) to accommodate this.

    #[test]
    fn duty_cycle_approx_half_strip() {
        let frame = render(&spec(), &strip(), 0.0);
        let on_count = frame.iter().filter(|&&c| c == [255u8, 255, 255]).count();
        assert!(
            (64..=68).contains(&on_count),
            "on-count should be ≈66 (half of 132), got {on_count}"
        );
    }

    #[test]
    fn duty_cycle_approx_half_fans() {
        let frame = render(&spec(), &fans(), 0.0);
        let on_count = frame.iter().filter(|&&c| c == [255u8, 255, 255]).count();
        // Fans{3,44}: 132 total LEDs; same duty-cycle analysis applies.
        assert!(
            (64..=68).contains(&on_count),
            "on-count should be ≈66 (half of 132), got {on_count}"
        );
    }

    // ---- custom colors: on=red, off=blue ----

    #[test]
    fn custom_colors_respected() {
        let spec_custom = EffectSpec {
            kind: EffectKind::Runway,
            colors: vec![[255, 0, 0], [0, 0, 255]],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        };
        let frame = render(&spec_custom, &strip(), 0.0);
        // Every LED must be either red (on) or blue (off), never anything else.
        for (i, &c) in frame.iter().enumerate() {
            assert!(
                c == [255, 0, 0] || c == [0, 0, 255],
                "LED {i} must be red or blue, got {c:?}"
            );
        }
        let red_count  = frame.iter().filter(|&&c| c == [255u8, 0, 0]).count();
        let blue_count = frame.iter().filter(|&&c| c == [0u8, 0, 255]).count();
        assert_eq!(red_count + blue_count, 132, "all LEDs should be accounted for");
    }

    // ---- single color: on=red, off=black ----

    #[test]
    fn single_color_off_is_black() {
        let spec_one = EffectSpec {
            kind: EffectKind::Runway,
            colors: vec![[255, 0, 0]],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        };
        let frame = render(&spec_one, &strip(), 0.0);
        for (i, &c) in frame.iter().enumerate() {
            assert!(
                c == [255, 0, 0] || c == [0, 0, 0],
                "LED {i} must be red (on) or black (off), got {c:?}"
            );
        }
    }

    // ---- segments march over time ----
    //
    // At t=0 and t=1/3, the pattern shifts: (pos*6 + t*3) changes by t*3.
    // At t=1/3: shift = 1/3*3 = 1.0 → mod 1 = 0 → same pattern as t=0?
    // Actually at t=1/6: shift = 1/6*3 = 0.5 → pattern inverts: on/off swap.
    // LED 0 at t=0: (0*6+0) mod 1 = 0 < 0.5 → ON
    // LED 0 at t=1/6: (0*6 + 1/6*3) mod 1 = 0.5 mod 1 = 0.5 → NOT < 0.5 → OFF

    #[test]
    fn segments_march_over_time() {
        let s = spec();
        let g = strip();
        let frame_t0 = render(&s, &g, 0.0);
        // LED 0 is ON at t=0 (d=0 < 0.5)
        assert_eq!(frame_t0[0], [255, 255, 255], "LED 0 at t=0 should be ON");
        // At t just above 1/6, the shift makes LED 0 go OFF
        // t=1/6+ε: (0*6 + (1/6+ε)*3) mod 1 = (0.5+ε*3) mod 1 = 0.5+ε*3 ≥ 0.5 → OFF
        let frame_t_shift = render(&s, &g, 1.0 / 6.0 + 0.01);
        assert_eq!(
            frame_t_shift[0], [0, 0, 0],
            "LED 0 at t=1/6+ε should be OFF (shifted past threshold)"
        );
    }

    // ---- direction: Reverse frame(t) equals Forward frame rendered with t→1-t ----
    //
    // render_animation with Reverse maps t → 1-t for directional effects.
    // Runway.directional() = true, so:
    //   Reverse frame i (of 24) → t_eff = 1 - i/24
    //   Forward frame (24-i) → t = (24-i)/24 = 1 - i/24
    // So Reverse[i] == Forward[24-i] for i>0, and Reverse[0] == Forward[0] at t=1.0
    // (which wraps like t=0 due to rem_euclid, or is handled as t=1.0 which the
    // formula also handles correctly since mod applies).
    //
    // We verify: Reverse frame 6 == Forward frame 18.
    //   Reverse frame 6: t_eff = 1 - 6/24 = 0.75
    //   Forward frame 18: t = 18/24 = 0.75
    // These are identical render calls → identical frames.

    #[test]
    fn runway_direction_reverse_maps_t() {
        use crate::{render_animation, Direction};
        let geom = strip();
        let frames = 24u16;

        let spec_fwd = EffectSpec {
            kind: EffectKind::Runway,
            colors: vec![],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        };
        let (fwd, _) = render_animation(&spec_fwd, &geom, frames);

        let spec_rev = EffectSpec {
            kind: EffectKind::Runway,
            colors: vec![],
            speed: 3,
            direction: Direction::Reverse,
            brightness: 4,
        };
        let (rev, _) = render_animation(&spec_rev, &geom, frames);

        assert_eq!(
            rev[6], fwd[18],
            "Reverse frame 6 (t_eff=0.75) must equal Forward frame 18 (t=0.75)"
        );
    }
}
