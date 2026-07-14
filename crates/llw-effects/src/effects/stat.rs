use crate::{color, EffectSpec};
use crate::geometry::Geometry;

/// Static: every LED takes `colors[0]`, defaulting to white when the palette
/// is empty.
pub fn render(spec: &EffectSpec, geom: &Geometry, _t: f32) -> Vec<[u8; 3]> {
    let color = color::palette(&spec.colors, 0.0);
    vec![color; geom.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectKind, Direction};

    fn spec_with_colors(colors: Vec<[u8; 3]>) -> EffectSpec {
        EffectSpec {
            kind: EffectKind::Static,
            colors,
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    #[test]
    fn static_default_white() {
        let spec = spec_with_colors(vec![]);
        let geom = Geometry::Fans { fan_count: 3, leds_per_fan: 44 };
        let frame = render(&spec, &geom, 0.0);
        assert_eq!(frame.len(), 132);
        assert!(frame.iter().all(|&c| c == [255, 255, 255]));
    }

    #[test]
    fn static_custom_color() {
        let col = [10u8, 20, 30];
        let spec = spec_with_colors(vec![col]);
        let geom = Geometry::Strip { total: 50 };
        let frame = render(&spec, &geom, 0.5);
        assert_eq!(frame.len(), 50);
        assert!(frame.iter().all(|&c| c == col));
    }

    #[test]
    fn static_is_time_invariant() {
        let spec = spec_with_colors(vec![[255, 0, 0]]);
        let geom = Geometry::Fans { fan_count: 1, leds_per_fan: 8 };
        let f0 = render(&spec, &geom, 0.0);
        let f1 = render(&spec, &geom, 0.99);
        assert_eq!(f0, f1);
    }
}
