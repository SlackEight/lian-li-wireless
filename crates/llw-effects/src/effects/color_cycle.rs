//! ColorCycle effect — the whole device fades smoothly through the palette.
//!
//! # Algorithm
//!
//! ```text
//! LED color = palette(t)
//! ```
//!
//! Every LED shows the same colour at each phase `t`, and that colour walks
//! continuously through the configured palette over one period.  With the
//! default empty palette every frame is white (palette's empty-slice default).

use crate::{color, EffectSpec};
use crate::geometry::Geometry;

/// Render one frame of the ColorCycle effect at phase `t ∈ [0, 1)`.
///
/// All LEDs receive `palette(t)` — uniform across every geometry.
pub fn render(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    let led = color::palette(&spec.colors, t);
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
            kind: EffectKind::ColorCycle,
            colors,
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
        let frame = render(&spec(vec![]), &fans(), 0.4);
        let first = frame[0];
        assert!(frame.iter().all(|&c| c == first), "all LEDs must be equal (fans)");
    }

    #[test]
    fn all_leds_equal_strip() {
        let frame = render(&spec(vec![]), &strip(), 0.4);
        let first = frame[0];
        assert!(frame.iter().all(|&c| c == first), "all LEDs must be equal (strip)");
    }

    // ---- golden values — default white palette ----
    //
    // palette([], t) = [255, 255, 255]   (white — constant regardless of t)
    //
    // t = 0.0 : palette([], 0.0)  = [255, 255, 255]
    // t = 0.25: palette([], 0.25) = [255, 255, 255]
    // t = 0.5 : palette([], 0.5)  = [255, 255, 255]

    #[test]
    fn golden_fans_t0() {
        let frame = render(&spec(vec![]), &fans(), 0.0);
        assert_eq!(frame[0],   [255, 255, 255], "LED 0   t=0 fans");
        assert_eq!(frame[65],  [255, 255, 255], "LED 65  t=0 fans");
        assert_eq!(frame[131], [255, 255, 255], "LED 131 t=0 fans");
    }

    #[test]
    fn golden_fans_t025() {
        let frame = render(&spec(vec![]), &fans(), 0.25);
        assert_eq!(frame[0],   [255, 255, 255], "LED 0   t=0.25 fans");
        assert_eq!(frame[65],  [255, 255, 255], "LED 65  t=0.25 fans");
        assert_eq!(frame[131], [255, 255, 255], "LED 131 t=0.25 fans");
    }

    #[test]
    fn golden_fans_t05() {
        let frame = render(&spec(vec![]), &fans(), 0.5);
        assert_eq!(frame[0],   [255, 255, 255], "LED 0   t=0.5 fans");
        assert_eq!(frame[65],  [255, 255, 255], "LED 65  t=0.5 fans");
        assert_eq!(frame[131], [255, 255, 255], "LED 131 t=0.5 fans");
    }

    #[test]
    fn golden_strip_t0() {
        let frame = render(&spec(vec![]), &strip(), 0.0);
        assert_eq!(frame[0],   [255, 255, 255], "LED 0   t=0 strip");
        assert_eq!(frame[65],  [255, 255, 255], "LED 65  t=0 strip");
        assert_eq!(frame[131], [255, 255, 255], "LED 131 t=0 strip");
    }

    #[test]
    fn golden_strip_t025() {
        let frame = render(&spec(vec![]), &strip(), 0.25);
        assert_eq!(frame[0],   [255, 255, 255], "LED 0   t=0.25 strip");
        assert_eq!(frame[65],  [255, 255, 255], "LED 65  t=0.25 strip");
        assert_eq!(frame[131], [255, 255, 255], "LED 131 t=0.25 strip");
    }

    #[test]
    fn golden_strip_t05() {
        let frame = render(&spec(vec![]), &strip(), 0.5);
        assert_eq!(frame[0],   [255, 255, 255], "LED 0   t=0.5 strip");
        assert_eq!(frame[65],  [255, 255, 255], "LED 65  t=0.5 strip");
        assert_eq!(frame[131], [255, 255, 255], "LED 131 t=0.5 strip");
    }

    // ---- with a two-color palette — verify palette interpolation ----
    //
    // colors = [[255,0,0], [0,0,255]]
    // palette splits [0,1) into 2 segments:
    //   [0, 0.5) → segment 0: lerp(red, blue, frac*2)
    //   [0.5, 1) → segment 1: lerp(blue, red, frac*2)  (wraparound)
    //
    // t = 0.0 : i=0, frac=0 → red
    // t = 0.25: i=0, frac=0.5 → lerp(red,blue,0.5) = [128,0,128]
    // t = 0.5 : i=1, frac=0 → blue

    #[test]
    fn golden_two_color_t0() {
        let colors = vec![[255u8, 0, 0], [0, 0, 255]];
        let frame = render(&spec(colors), &fans(), 0.0);
        assert_eq!(frame[0],   [255, 0, 0], "t=0 → red");
        assert_eq!(frame[131], [255, 0, 0], "t=0 → red (last LED)");
    }

    #[test]
    fn golden_two_color_t025() {
        // t=0.25: scaled=0.5, i=0, frac=0.5
        // lerp([255,0,0],[0,0,255],0.5): r=(255+(-255)*0.5).round()=128, g=0, b=(0+255*0.5).round()=128
        let colors = vec![[255u8, 0, 0], [0, 0, 255]];
        let frame = render(&spec(colors), &fans(), 0.25);
        assert_eq!(frame[0], [128, 0, 128], "t=0.25 → mid red-blue");
    }

    #[test]
    fn golden_two_color_t05() {
        let colors = vec![[255u8, 0, 0], [0, 0, 255]];
        let frame = render(&spec(colors), &fans(), 0.5);
        assert_eq!(frame[0], [0, 0, 255], "t=0.5 → blue");
    }
}
