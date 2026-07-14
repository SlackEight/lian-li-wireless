/// Physical LED layout for fan devices.
///
/// `UniformRing` is the default assumption — LEDs are evenly distributed
/// around the ring in a single circular chain. `SlInf44` encodes the empirically
/// measured 5-segment wiring of the SL-INF (44 LEDs/fan), chase-probed 2026-07-14.
///
/// Adding a new layout: add a variant, implement the corresponding arm in
/// [`led_polar`], and update all `Fans { .. }` construction sites to specify the
/// layout. The `#[serde(default)]` attribute means old config files round-trip
/// cleanly as `UniformRing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanLayout {
    /// LEDs are uniformly spaced around a single ring (prior assumption).
    #[default]
    UniformRing,
    /// SL-INF 44-LED/fan measured wiring — 5 segments, chase-probed.
    ///
    /// Segments (k = index within segment):
    ///
    /// | idx   | segment          | count | angle formula                                            | radius |
    /// |-------|------------------|-------|----------------------------------------------------------|--------|
    /// | 0-7   | inner ring       | 8     | `(0.75 + k/8) mod 1` (clockwise from left-middle)       | 0.7    |
    /// | 8-17  | outer LEFT arc   | 10    | `(0.5 + (k+0.5)×0.05) mod 1` (bottom→top)               | 1.0    |
    /// | 18-25 | LEFT side strip  | 8     | `(0.5 + (k+0.5)×0.0625) mod 1` (bottom→top)             | 1.15   |
    /// | 26-35 | outer RIGHT arc  | 10    | `(0.5 − (k+0.5)×0.05).rem_euclid(1)` (bottom→top)      | 1.0    |
    /// | 36-43 | RIGHT side strip | 8     | `(0.5 − (k+0.5)×0.0625).rem_euclid(1)` (bottom→top)    | 1.15   |
    ///
    /// Angle convention: 0 = top, clockwise, fractional turns.
    /// Max physical radius is 1.15 (side strips); normalise by 1.15 for effects.
    SlInf44,
}

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
    ///   - ring angle  `a = i / L`          ∈ [0, 1)    (UniformRing)
    ///   - chain position `c = (f + i/L) / N` ∈ [0, 1)
    ///   - polar coords  `(angle, radius)` via [`led_polar`] (layout-specific)
    Fans {
        fan_count: u8,
        leds_per_fan: u8,
        /// Physical LED wiring layout. Defaults to `UniformRing` for
        /// backwards-compat with existing construction sites.
        #[serde(default)]
        layout: FanLayout,
    },
    /// Flat strip; LED `i` has position `p = i / total` ∈ [0, 1).
    Strip { total: u16 },
}

impl Geometry {
    /// Total number of LEDs.
    pub fn len(&self) -> usize {
        match self {
            Geometry::Fans { fan_count, leds_per_fan, .. } => {
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
///
/// # Preconditions
///
/// `leds_per_fan` must be greater than 0.
#[inline]
pub fn ring_angle(i: u8, leds_per_fan: u8) -> f32 {
    debug_assert!(leds_per_fan > 0, "leds_per_fan must be > 0");
    i as f32 / leds_per_fan as f32
}

/// Chain position for LED `i` in fan `fan` across a device with `fan_count`
/// fans and `leds_per_fan` LEDs per fan.
///
/// Returns `(fan + i/leds_per_fan) / fan_count` ∈ [0, 1).
///
/// # Preconditions
///
/// Both `fan_count` and `leds_per_fan` must be greater than 0.
#[inline]
pub fn chain_pos(fan: u8, i: u8, fan_count: u8, leds_per_fan: u8) -> f32 {
    debug_assert!(fan_count > 0, "fan_count must be > 0");
    debug_assert!(leds_per_fan > 0, "leds_per_fan must be > 0");
    (fan as f32 + i as f32 / leds_per_fan as f32) / fan_count as f32
}

/// Linear position for LED `i` in a strip of `total` LEDs.
///
/// Returns `i / total` ∈ [0, 1).
///
/// # Preconditions
///
/// `total` must be greater than 0.
#[inline]
pub fn strip_pos(i: u16, total: u16) -> f32 {
    debug_assert!(total > 0, "total must be > 0");
    i as f32 / total as f32
}

/// Polar coordinates `(angle, radius)` for LED index `i` within a fan.
///
/// - `angle` ∈ [0, 1): fractional turns, 0 = top, clockwise.
/// - `radius`: relative radius. `UniformRing` → always 1.0. `SlInf44` →
///   inner ring 0.7, arcs 1.0, side strips 1.15. Normalise by 1.15 for
///   effects that want a 0..=1 radial distance.
///
/// For `SlInf44`, `leds_per_fan` must be 44 in debug builds; in release builds
/// it falls back to `UniformRing` math for any other count.
pub fn led_polar(layout: FanLayout, i: u8, leds_per_fan: u8) -> (f32, f32) {
    match layout {
        FanLayout::UniformRing => (ring_angle(i, leds_per_fan), 1.0),
        FanLayout::SlInf44 => {
            debug_assert_eq!(
                leds_per_fan, 44,
                "SlInf44 layout requires leds_per_fan=44, got {leds_per_fan}"
            );
            if leds_per_fan != 44 {
                // Release-build fallback: treat as uniform ring.
                return (ring_angle(i, leds_per_fan), 1.0);
            }
            sl_inf44_polar(i)
        }
    }
}

/// Compute the polar coordinates for LED `i` within an SL-INF 44-LED fan.
///
/// Radius values are VISUAL-TIMING values tuned on hardware 2026-07-14
/// (inner 0.7 ≈ wave reaches outer ring ~0.8 s after inner flash at speed 3),
/// not physical measurements.
///
/// Segments and their formulae (k = index within segment):
///
/// | idx   | segment          | angle formula                              | radius |
/// |-------|------------------|--------------------------------------------|--------|
/// | 0-7   | inner ring       | `(0.75 + k/8.0) mod 1`                    | 0.7    |
/// | 8-17  | outer LEFT arc   | `(0.5 + (k+0.5)×0.05) mod 1`             | 1.0    |
/// | 18-25 | LEFT side strip  | `(0.5 + (k+0.5)×0.0625) mod 1`           | 1.15   |
/// | 26-35 | outer RIGHT arc  | `(0.5 − (k+0.5)×0.05).rem_euclid(1)`    | 1.0    |
/// | 36-43 | RIGHT side strip | `(0.5 − (k+0.5)×0.0625).rem_euclid(1)`  | 1.15   |
#[inline]
fn sl_inf44_polar(i: u8) -> (f32, f32) {
    match i {
        // 0-7: inner ring — 8 LEDs, clockwise from left-middle (angle 0.75)
        0..=7 => {
            let k = i as f32;
            let angle = (0.75 + k / 8.0).rem_euclid(1.0);
            (angle, 0.7)
        }
        // 8-17: outer LEFT arc — 10 LEDs, bottom→top
        8..=17 => {
            let k = (i - 8) as f32;
            let angle = (0.5 + (k + 0.5) * 0.05).rem_euclid(1.0);
            (angle, 1.0)
        }
        // 18-25: LEFT side strip — 8 LEDs, bottom→top
        18..=25 => {
            let k = (i - 18) as f32;
            let angle = (0.5 + (k + 0.5) * 0.0625).rem_euclid(1.0);
            (angle, 1.15)
        }
        // 26-35: outer RIGHT arc — 10 LEDs, bottom→top
        26..=35 => {
            let k = (i - 26) as f32;
            let angle = (0.5 - (k + 0.5) * 0.05).rem_euclid(1.0);
            (angle, 1.0)
        }
        // 36-43: RIGHT side strip — 8 LEDs, bottom→top
        36..=43 => {
            let k = (i - 36) as f32;
            let angle = (0.5 - (k + 0.5) * 0.0625).rem_euclid(1.0);
            (angle, 1.15)
        }
        // Unreachable for a 44-LED fan (asserted by caller), but needed for exhaustiveness.
        _ => (ring_angle(i, 44), 1.0),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_fans() {
        let g = Geometry::Fans { fan_count: 3, leds_per_fan: 44, layout: FanLayout::UniformRing };
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

    // ---------------------------------------------------------------------------
    // led_polar / SlInf44 table pin tests
    //
    // All computed from sl_inf44_polar() table (chase-probed 2026-07-14).
    // Tolerance 1e-5 covers f32 rounding in the formula.
    // ---------------------------------------------------------------------------

    #[test]
    fn led_polar_uniform_ring_is_ring_angle() {
        // UniformRing: angle == ring_angle(i, leds_per_fan), radius always 1.0.
        for i in 0..44u8 {
            let (angle, radius) = led_polar(FanLayout::UniformRing, i, 44);
            assert!((angle - ring_angle(i, 44)).abs() < 1e-6, "UniformRing angle mismatch at i={i}");
            assert!((radius - 1.0).abs() < 1e-6, "UniformRing radius must be 1.0 at i={i}");
        }
    }

    #[test]
    fn sl_inf44_idx0_inner_ring_start() {
        // idx 0: inner ring, k=0. angle = (0.75 + 0/8).rem_euclid(1) = 0.75. radius = 0.7.
        let (angle, radius) = led_polar(FanLayout::SlInf44, 0, 44);
        assert!((angle - 0.75).abs() < 1e-5, "idx 0 angle must be 0.75, got {angle}");
        assert!((radius - 0.7).abs() < 1e-5, "idx 0 radius must be 0.7, got {radius}");
    }

    #[test]
    fn sl_inf44_idx7_inner_ring_end() {
        // idx 7: inner ring, k=7. angle = (0.75 + 7/8.0).rem_euclid(1) = (0.75+0.875) mod 1 = 0.625. radius = 0.7.
        let (angle, radius) = led_polar(FanLayout::SlInf44, 7, 44);
        assert!((angle - 0.625).abs() < 1e-5, "idx 7 angle must be 0.625, got {angle}");
        assert!((radius - 0.7).abs() < 1e-5, "idx 7 radius must be 0.7, got {radius}");
    }

    #[test]
    fn sl_inf44_idx8_left_arc_start() {
        // idx 8: outer LEFT arc, k=0. angle = (0.5 + 0.5×0.05) = 0.525. radius = 1.0.
        let (angle, radius) = led_polar(FanLayout::SlInf44, 8, 44);
        assert!((angle - 0.525).abs() < 1e-5, "idx 8 angle must be 0.525, got {angle}");
        assert!((radius - 1.0).abs() < 1e-5, "idx 8 radius must be 1.0, got {radius}");
    }

    #[test]
    fn sl_inf44_idx17_left_arc_end() {
        // idx 17: outer LEFT arc, k=9. angle = (0.5 + 9.5×0.05) = (0.5+0.475) = 0.975. radius = 1.0.
        let (angle, radius) = led_polar(FanLayout::SlInf44, 17, 44);
        assert!((angle - 0.975).abs() < 1e-5, "idx 17 angle must be 0.975, got {angle}");
        assert!((radius - 1.0).abs() < 1e-5, "idx 17 radius must be 1.0, got {radius}");
    }

    #[test]
    fn sl_inf44_idx26_right_arc_start() {
        // idx 26: outer RIGHT arc, k=0. angle = (0.5 − 0.5×0.05).rem_euclid(1) = 0.475. radius = 1.0.
        let (angle, radius) = led_polar(FanLayout::SlInf44, 26, 44);
        assert!((angle - 0.475).abs() < 1e-5, "idx 26 angle must be 0.475, got {angle}");
        assert!((radius - 1.0).abs() < 1e-5, "idx 26 radius must be 1.0, got {radius}");
    }

    #[test]
    fn sl_inf44_idx43_right_strip_end() {
        // idx 43: RIGHT side strip, k=7. angle = (0.5 − 7.5×0.0625).rem_euclid(1) = (0.5−0.46875) = 0.03125. radius = 1.15.
        let (angle, radius) = led_polar(FanLayout::SlInf44, 43, 44);
        assert!((angle - 0.03125).abs() < 1e-5, "idx 43 angle must be 0.03125, got {angle}");
        assert!((radius - 1.15).abs() < 1e-5, "idx 43 radius must be 1.15, got {radius}");
    }

    #[test]
    fn sl_inf44_segment_radii() {
        // Verify segment radii (visual-timing values tuned 2026-07-14): inner=0.7, arcs=1.0, strips=1.15.
        let (_, r_inner) = led_polar(FanLayout::SlInf44, 0, 44);   // inner ring
        let (_, r_arc)   = led_polar(FanLayout::SlInf44, 8, 44);   // left arc
        let (_, r_strip) = led_polar(FanLayout::SlInf44, 18, 44);  // left strip
        assert!((r_inner - 0.7 ).abs() < 1e-5, "inner ring radius must be 0.7");
        assert!((r_arc   - 1.0 ).abs() < 1e-5, "arc radius must be 1.0");
        assert!((r_strip - 1.15).abs() < 1e-5, "strip radius must be 1.15");
    }
}
