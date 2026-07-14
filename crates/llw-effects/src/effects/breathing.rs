//! Breathing effect — the whole device pulses in brightness together.
//!
//! # Algorithm
//!
//! ```text
//! raw_color = palette(t)
//! v         = sin²(π · t)
//! LED color = scale(raw_color, v)
//! ```
//!
//! Because `palette(t)` is evaluated at the current phase, using a
//! **single-color** palette produces a clean, uniform breath — the hue stays
//! constant and only the brightness moves.  With a **multi-color** palette the
//! color drifts continuously across the breath; at `t ≈ 0.99` a two-color
//! palette blends the last color back toward the first (the palette wraparound
//! seam — `palette` is designed to loop seamlessly).  This drift is intentional
//! per the M3 design decision: v1 single-period animations cannot track
//! inter-period state, so palette position and brightness phase are unified into
//! `t`.  Visual tuning (e.g. splitting color steps to whole periods) is deferred
//! to hardware experimentation post-gate.

use crate::{color, EffectSpec};
use crate::geometry::Geometry;

/// Render one frame of the Breathing effect at phase `t ∈ [0, 1)`.
///
/// All LEDs receive the same colour (`palette(t)` × `sin²(π·t)`) — uniform
/// across every geometry.
pub fn render(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    let base = color::palette(&spec.colors, t);
    let v = (std::f32::consts::PI * t).sin().powi(2);
    let led = color::scale(base, v);
    vec![led; geom.len()]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectKind, Direction};

    fn spec(colors: Vec<[u8; 3]>) -> EffectSpec {
        EffectSpec {
            kind: EffectKind::Breathing,
            colors,
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    // ---- helpers ----

    fn fans() -> Geometry { Geometry::Fans { fan_count: 3, leds_per_fan: 44, layout: crate::geometry::FanLayout::UniformRing } }
    fn strip() -> Geometry { Geometry::Strip { total: 132 } }

    // ---- uniformity ----

    #[test]
    fn all_leds_equal_fans() {
        let frame = render(&spec(vec![]), &fans(), 0.3);
        let first = frame[0];
        assert!(frame.iter().all(|&c| c == first), "all LEDs must be equal (fans)");
    }

    #[test]
    fn all_leds_equal_strip() {
        let frame = render(&spec(vec![]), &strip(), 0.3);
        let first = frame[0];
        assert!(frame.iter().all(|&c| c == first), "all LEDs must be equal (strip)");
    }

    // ---- golden values — default white palette ----
    //
    // palette([], t) = [255, 255, 255]   (white — constant regardless of t)
    // v = sin²(π · t);  scale([255,255,255], v) rounds each channel
    //
    // t = 0.0 : sin²(0)    = 0.0  → scale([255,255,255], 0.0) = [0, 0, 0]
    //
    // t = 0.25: sin²(π/4)  in real-number math = (√2/2)² = 0.5 exactly.
    //   In f32, `(std::f32::consts::PI * 0.25_f32).sin().powi(2)` = 0.49999997…
    //   (the last f32 below 0.5).  Therefore 255 × 0.4999… ≈ 127.4999, which
    //   .round() produces **127**, not 128.  This is the correct independent
    //   arithmetic at f32 precision — do not adjust the implementation.
    //
    // t = 0.5 : sin²(π/2)  = 1.0  → scale([255,255,255], 1.0) = [255, 255, 255]

    #[test]
    fn golden_fans_t0() {
        // t = 0 → sin²(0) = 0 → all black
        let frame = render(&spec(vec![]), &fans(), 0.0);
        assert_eq!(frame[0],   [0, 0, 0], "LED 0   t=0 fans");
        assert_eq!(frame[65],  [0, 0, 0], "LED 65  t=0 fans");
        assert_eq!(frame[131], [0, 0, 0], "LED 131 t=0 fans");
    }

    #[test]
    fn golden_fans_t025() {
        // t = 0.25 → v = sin²(π/4) ≈ 0.49999997 (f32) → (255 × 0.4999…).round() = 127
        // (see comment block above for why this differs from the exact real-number result)
        let frame = render(&spec(vec![]), &fans(), 0.25);
        assert_eq!(frame[0],   [127, 127, 127], "LED 0   t=0.25 fans");
        assert_eq!(frame[65],  [127, 127, 127], "LED 65  t=0.25 fans");
        assert_eq!(frame[131], [127, 127, 127], "LED 131 t=0.25 fans");
    }

    #[test]
    fn golden_fans_t05() {
        // t = 0.5 → sin²(π/2) = 1.0 → [255,255,255]
        let frame = render(&spec(vec![]), &fans(), 0.5);
        assert_eq!(frame[0],   [255, 255, 255], "LED 0   t=0.5 fans");
        assert_eq!(frame[65],  [255, 255, 255], "LED 65  t=0.5 fans");
        assert_eq!(frame[131], [255, 255, 255], "LED 131 t=0.5 fans");
    }

    #[test]
    fn golden_strip_t0() {
        let frame = render(&spec(vec![]), &strip(), 0.0);
        assert_eq!(frame[0],   [0, 0, 0], "LED 0   t=0 strip");
        assert_eq!(frame[65],  [0, 0, 0], "LED 65  t=0 strip");
        assert_eq!(frame[131], [0, 0, 0], "LED 131 t=0 strip");
    }

    #[test]
    fn golden_strip_t025() {
        // Same f32 precision: (255 × sin²(π/4_f32)).round() = 127
        let frame = render(&spec(vec![]), &strip(), 0.25);
        assert_eq!(frame[0],   [127, 127, 127], "LED 0   t=0.25 strip");
        assert_eq!(frame[65],  [127, 127, 127], "LED 65  t=0.25 strip");
        assert_eq!(frame[131], [127, 127, 127], "LED 131 t=0.25 strip");
    }

    #[test]
    fn golden_strip_t05() {
        let frame = render(&spec(vec![]), &strip(), 0.5);
        assert_eq!(frame[0],   [255, 255, 255], "LED 0   t=0.5 strip");
        assert_eq!(frame[65],  [255, 255, 255], "LED 65  t=0.5 strip");
        assert_eq!(frame[131], [255, 255, 255], "LED 131 t=0.5 strip");
    }

    // ---- t=0 is dark (breath starts closed) ----

    #[test]
    fn breath_dark_at_t0() {
        let frame = render(&spec(vec![]), &fans(), 0.0);
        assert!(frame.iter().all(|&c| c == [0, 0, 0]), "breath must be black at t=0");
    }

    // ---- peak brightness at t=0.5 ----

    #[test]
    fn breath_peak_at_t05() {
        let frame = render(&spec(vec![]), &fans(), 0.5);
        assert!(frame.iter().all(|&c| c == [255, 255, 255]), "breath peak at t=0.5 with white");
    }
}
