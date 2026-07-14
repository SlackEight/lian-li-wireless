/// Abstract device geometry used by effect renderers.
///
/// Frame layout mirrors the wire format: fans concatenated fan0..fanN in ring
/// order; strip is linear. `Geometry::len()` gives the total LED count.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Geometry {
    /// N fans × L LEDs per fan.
    ///
    /// LED `i` of fan `f` has:
    ///   - ring angle  `a = i / L`          ∈ [0, 1)
    ///   - chain position `c = (f + i/L) / N` ∈ [0, 1)
    Fans { fan_count: u8, leds_per_fan: u8 },
    /// Flat strip; LED `i` has position `p = i / total` ∈ [0, 1).
    Strip { total: u16 },
}

impl Geometry {
    /// Total number of LEDs.
    pub fn len(&self) -> usize {
        match self {
            Geometry::Fans { fan_count, leds_per_fan } => {
                *fan_count as usize * *leds_per_fan as usize
            }
            Geometry::Strip { total } => *total as usize,
        }
    }

    /// Returns `true` if there are no LEDs.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// Position helpers — all return f32 in [0, 1)
// ---------------------------------------------------------------------------

/// Ring angle for LED index `i` within a fan that has `leds_per_fan` LEDs.
///
/// Returns `i / leds_per_fan` ∈ [0, 1).
#[inline]
pub fn ring_angle(i: u8, leds_per_fan: u8) -> f32 {
    i as f32 / leds_per_fan as f32
}

/// Chain position for LED `i` in fan `fan` across a device with `fan_count`
/// fans and `leds_per_fan` LEDs per fan.
///
/// Returns `(fan + i/leds_per_fan) / fan_count` ∈ [0, 1).
#[inline]
pub fn chain_pos(fan: u8, i: u8, fan_count: u8, leds_per_fan: u8) -> f32 {
    (fan as f32 + i as f32 / leds_per_fan as f32) / fan_count as f32
}

/// Linear position for LED `i` in a strip of `total` LEDs.
///
/// Returns `i / total` ∈ [0, 1).
#[inline]
pub fn strip_pos(i: u16, total: u16) -> f32 {
    i as f32 / total as f32
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_fans() {
        let g = Geometry::Fans { fan_count: 3, leds_per_fan: 44 };
        assert_eq!(g.len(), 132);
    }

    #[test]
    fn len_strip() {
        let g = Geometry::Strip { total: 200 };
        assert_eq!(g.len(), 200);
    }

    #[test]
    fn ring_angle_boundaries() {
        // First LED is always angle 0.
        assert_eq!(ring_angle(0, 44), 0.0);
        // Last LED is (L-1)/L, strictly < 1.
        let last = ring_angle(43, 44);
        assert!(last < 1.0, "last angle must be < 1.0, got {last}");
        assert!((last - 43.0_f32 / 44.0).abs() < 1e-6);
    }

    #[test]
    fn ring_angle_single_fan() {
        // With 1 LED per fan the only valid index is 0.
        assert_eq!(ring_angle(0, 1), 0.0);
    }

    #[test]
    fn chain_pos_boundaries() {
        // First LED of first fan → 0.
        assert_eq!(chain_pos(0, 0, 3, 44), 0.0);
        // Last LED of last fan → (2 + 43/44) / 3 = (2 + 0.977..) / 3 < 1.
        let last = chain_pos(2, 43, 3, 44);
        assert!(last < 1.0, "last chain pos must be < 1.0, got {last}");
        let expected = (2.0_f32 + 43.0 / 44.0) / 3.0;
        assert!((last - expected).abs() < 1e-6);
    }

    #[test]
    fn chain_pos_single_fan() {
        // With one fan the chain pos equals the ring angle.
        let a = ring_angle(10, 44);
        let c = chain_pos(0, 10, 1, 44);
        assert!((c - a).abs() < 1e-6, "single-fan chain pos must equal ring angle");
    }

    #[test]
    fn strip_pos_boundaries() {
        assert_eq!(strip_pos(0, 100), 0.0);
        let last = strip_pos(99, 100);
        assert!(last < 1.0);
        assert!((last - 0.99_f32).abs() < 1e-6);
    }
}
