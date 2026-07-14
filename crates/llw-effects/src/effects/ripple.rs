//! Ripple effect — a pulse expands outward from the origin, fading as it travels.
//!
//! # Algorithm
//!
//! ```text
//! r    = t                                     -- wavefront radius ∈ [0, 1)
//! σ    = 0.06                                  -- Gaussian width (LOCKED v1)
//! fade = 1 − t                                 -- amplitude envelope (dies at t→1)
//!
//! brightness = exp(−(dist − r)² / (2·σ²)) · (1 − t)
//! color      = palette(t)                      -- hue tracks the wave phase
//! background = black
//! ```
//!
//! **Fans** (`ring_angle`-based, all fans in phase):
//!
//! ```text
//! a    = i / leds_per_fan                     -- ring angle ∈ [0, 1)
//! dist = min(a, 1 − a)                        -- shortest angular distance to angle 0
//! ```
//!
//! Because `dist` depends only on the LED index within a fan (not on which fan),
//! all fans render identically — the whole cluster pulses together.
//!
//! **Strip** (center-out):
//!
//! ```text
//! pos  = i / total                            -- linear position ∈ [0, 1)
//! dist = |pos − 0.5| · 2                     -- distance from center; ∈ [0, 1]
//! ```
//!
//! # Gaussian constants (σ = 0.06)
//!
//! At distance `d` from the wavefront:
//!
//! | |d|   | exp(−d²/(2·0.06²)) | × 255 (full amp) |
//! |-------|---------------------|------------------|
//! | 0.000 | 1.000               | 255              |
//! | 0.060 | 0.607               | 155              |
//! | 0.120 | 0.135               |  34              |
//! | 0.180 | 0.011               |   2.8 → ≤ 3      |
//! | 0.250 | 0.000169            |   < 0.1 → 0      |
//!
//! Beyond |d| ≈ 0.21, every channel rounds to 0 (effectively black).
//!
//! # Design notes
//!
//! The `(1 − t)` fade ensures one clean pulse per period with no wraparound
//! ghost: the ring brightens at t = 0, expands, and fully dies before the next
//! period begins at t → 1.  For Fans, the wavefront maximum `dist` is 0.5 (the
//! point diametrically opposite the origin), so the wave has crossed all LEDs
//! well before t = 0.5 — after that the amplitude envelope carries the dying
//! residue outward.  Ripple is non-directional (`EffectKind::directional()` is
//! false); direction has no visual meaning for a radial pulse.

use crate::{color, EffectSpec};
use crate::geometry::{self, FanLayout, Geometry};

/// Half-width of the Gaussian envelope in normalised distance units.
const SIGMA: f32 = 0.06;
/// Precomputed `2 · σ²` used in the exponent denominator.
const TWO_SIGMA_SQ: f32 = 2.0 * SIGMA * SIGMA; // = 0.0072

/// Render one frame of the **Ripple** effect at phase `t ∈ [0, 1)`.
///
/// Wavefront radius `r = t`.  LED brightness = `exp(−(dist − r)² / (2σ²)) · (1−t)`.
/// Color = `palette(t)`; background black.
pub fn render(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    let r = t;
    let fade = 1.0 - t;
    let base_color = color::palette(&spec.colors, t);

    match geom {
        Geometry::Fans { fan_count, leds_per_fan, layout } => {
            let fc = *fan_count;
            let lf = *leds_per_fan;
            let layout = *layout;
            let mut frame = Vec::with_capacity(fc as usize * lf as usize);
            for _fan in 0..fc {
                for i in 0..lf {
                    let dist = match layout {
                        FanLayout::UniformRing => {
                            // Angular distance: wavefront expands around the ring.
                            // dist depends only on ring angle (i), not on which fan → all fans identical.
                            let a = geometry::ring_angle(i, lf);
                            a.min(1.0 - a) // shortest angular distance to angle 0
                        }
                        FanLayout::SlInf44 => {
                            // Radial distance: wavefront expands outward from centre.
                            // Physical radius (raw): inner 0.4, arcs 1.0, strips 1.15.
                            // Normalise by 1.15 → inner ≈ 0.348, arcs ≈ 0.870, strips 1.0.
                            let (_, radius) = geometry::led_polar(layout, i, lf);
                            radius / 1.15
                        }
                    };
                    let brightness = (-(dist - r).powi(2) / TWO_SIGMA_SQ).exp() * fade;
                    frame.push(color::scale(base_color, brightness));
                }
            }
            frame
        }
        Geometry::Strip { total } => {
            let n = *total;
            (0..n)
                .map(|i| {
                    let pos = geometry::strip_pos(i, n);
                    let dist = (pos - 0.5).abs() * 2.0; // center-out; ∈ [0, 1]
                    let brightness = (-(dist - r).powi(2) / TWO_SIGMA_SQ).exp() * fade;
                    color::scale(base_color, brightness)
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
            kind: EffectKind::Ripple,
            colors: vec![],   // default white palette
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    fn fans() -> Geometry { Geometry::Fans { fan_count: 3, leds_per_fan: 44, layout: crate::geometry::FanLayout::UniformRing } }
    fn strip() -> Geometry { Geometry::Strip { total: 132 } }

    fn max_channel(frame: &[[u8; 3]]) -> u8 {
        frame.iter().flat_map(|c| c.iter().copied()).max().unwrap_or(0)
    }

    fn channel_sum(frame: &[[u8; 3]]) -> u64 {
        frame.iter().flat_map(|c| c.iter().map(|&v| v as u64)).sum()
    }

    // ---- t=0.1: brightest LED has dist within one LED-spacing of r=0.1 ----
    //
    // Fans{3,44}: ring_angle(i,44) = i/44; dist = min(i/44, 1−i/44).
    // Wavefront r = 0.1, fade = 0.9.
    // Gaussian peaks at dist = r = 0.1.
    // Closest LED: i=4 → dist = 4/44 ≈ 0.09091; |0.09091 − 0.1| = 0.00909 < 1/44 ≈ 0.02273.
    // brightness(LED 4) = exp(−(4/44−0.1)²/0.0072)·0.9 = exp(−0.00827/0.0072)·0.9 ≈ 0.8897
    // → 255 × 0.8897 ≈ 227.
    // Symmetry: LED 40 (a=40/44, dist=4/44) ties LED 4 — both are maximally bright.
    // LED spacing = 1/44 ≈ 0.02273.
    //
    // Strip{132}: dist = |pos−0.5|·2; dist-spacing between adjacent LEDs = 2/132 = 1/66.
    // dist=0.1 → pos=0.45 or 0.55 → i≈59.4 or 72.6.
    // LED 59: dist=|59/132−0.5|·2 = |−0.10303|·2 = 0.2060 … wait, recalculate:
    //   pos=59/132≈0.44697; |0.44697−0.5|=0.05303; ×2=0.10606.
    //   |0.10606−0.1|=0.00606 < 2/132≈0.01515. ✓  Brightest strip LED: i=59 or i=73.
    //   brightness ≈ exp(−(0.10606−0.1)²/0.0072)·0.9 = exp(−0.00036/0.0072)·0.9 ≈ 0.9 × 0.951 ≈ 0.856
    //   (LED 73: dist=|73/132−0.5|·2=|0.05303|·2=0.10606 — symmetric, same brightness)

    #[test]
    fn t01_brightest_near_wavefront_fans() {
        let frame = render(&spec(), &fans(), 0.1);
        let lf = 44u8;

        // Find the maximum channel value across the whole frame
        let peak = max_channel(&frame);

        // Find ANY LED that achieves this peak and check its dist is within 1 LED-spacing of r=0.1
        // LED-spacing = 1/44
        let led_spacing = 1.0_f32 / lf as f32; // ≈ 0.02273
        let r: f32 = 0.1;

        let mut found_near = false;
        for i in 0..lf {
            let a = i as f32 / lf as f32;
            let dist = a.min(1.0 - a);
            let idx = i as usize; // fan 0 suffices (all fans identical)
            if frame[idx].contains(&peak) {
                assert!(
                    (dist - r).abs() < led_spacing,
                    "brightest fan LED (i={i}, dist={dist:.5}) is more than one \
                     LED-spacing ({led_spacing:.5}) from wavefront r={r}"
                );
                found_near = true;
                break;
            }
        }
        assert!(found_near, "no LED in fan 0 matched the global peak {peak}");
    }

    #[test]
    fn t01_brightest_near_wavefront_strip() {
        let frame = render(&spec(), &strip(), 0.1);
        let total = 132u16;

        let peak = max_channel(&frame);

        // dist-spacing between adjacent strip LEDs = 2/132 = 1/66 ≈ 0.01515
        let dist_spacing = 2.0_f32 / total as f32; // 1/66
        let r: f32 = 0.1;

        let mut found_near = false;
        for i in 0..total {
            let pos = i as f32 / total as f32;
            let dist = (pos - 0.5).abs() * 2.0;
            if frame[i as usize].contains(&peak) {
                assert!(
                    (dist - r).abs() < dist_spacing,
                    "brightest strip LED (i={i}, dist={dist:.5}) is more than one \
                     dist-spacing ({dist_spacing:.5}) from wavefront r={r}"
                );
                found_near = true;
                break;
            }
        }
        assert!(found_near, "no strip LED matched the global peak {peak}");
    }

    // ---- t=0.9: for Strip, origin near-black; front carries faded pulse ----
    //
    // Strip{132}: dist = |pos−0.5|·2.
    // Origin = center LED 66 (pos=0.5, dist=0).
    //   brightness = exp(−(0−0.9)²/0.0072)·0.1 = exp(−112.5)·0.1 ≈ 0 → channel = 0.
    //
    // Front (wavefront r=0.9): dist=0.9 → pos=0.05 or 0.95.
    //   pos=0.05 → i≈6.6 → LED 6 (dist=0.90909) or LED 7 (dist=0.89394).
    //   LED 7: brightness = exp(−(0.89394−0.9)²/0.0072)·0.1 = exp(−0.000387/0.0072)·0.1
    //          = exp(−0.0538)·0.1 ≈ 0.9476·0.1 = 0.09476.
    //          channel = (255·0.09476).round() = 24 ≈ 255·(1−0.9) = 25.5.
    //   → peak ∈ [15, 30].
    //
    // Fans{3,44} at t=0.9: max fan dist = 0.5 (LED 22), but r=0.9 is beyond all fan LEDs.
    //   exp(−(0.5−0.9)²/0.0072)·0.1 = exp(−22.22)·0.1 ≈ 0 → all fans near-black.

    #[test]
    fn t09_origin_neardark_front_faded_strip() {
        let frame = render(&spec(), &strip(), 0.9);
        let total = 132usize;

        // Origin: center LED 66 (pos=0.5, dist=0) must be near-black (≤ 4)
        // exp(−0.81/0.0072)·0.1 = exp(−112.5)·0.1 ≈ 0
        let center = frame[total / 2]; // LED 66
        assert!(
            center.iter().all(|&v| v <= 4),
            "origin (LED 66, dist=0) must be near-black at t=0.9, got {center:?}"
        );

        // Front (dist≈0.9): expect peak channel ≈ 255·(1−0.9)·gaussian_peak ≈ 25.
        // Theoretical: exp(−(dist_best−0.9)²/0.0072)·0.1·255 ≈ 25.
        // Assert: peak channel across whole frame is in [15, 30].
        let peak = max_channel(&frame);
        assert!(
            (15..=30).contains(&peak),
            "strip peak channel at t=0.9 should be ≈25 (in [15,30]), got {peak}"
        );
    }

    #[test]
    fn t09_fans_all_near_black() {
        let frame = render(&spec(), &fans(), 0.9);
        // At t=0.9 the wavefront r=0.9 is beyond max fan dist=0.5.
        // exp(−(0.5−0.9)²/0.0072)·0.1 = exp(−22.22)·0.1 ≈ 2.2e-11 → all channels 0.
        assert!(
            frame.iter().all(|c| c.iter().all(|&v| v <= 4)),
            "all fan LEDs must be near-black at t=0.9 (wavefront beyond max fan dist)"
        );
    }

    // ---- energy die-out: sum of channels at t=0.95 < sum at t=0.5 ----
    //
    // The (1−t) fade envelope strictly reduces total energy over time.
    // At t=0.5: (1−t)=0.5; at t=0.95: (1−t)=0.05 → 10× smaller.
    // For Fans at t=0.95, r=0.95 > max dist=0.5 → all near-zero, energy≈0.
    // For Strip at t=0.95: wavefront near the edges; some small residue, but
    // (0.05/0.5)·same_gaussian < energy at t=0.5 by at least a factor of 10.

    #[test]
    fn energy_dieout_fans() {
        let e50  = channel_sum(&render(&spec(), &fans(),  0.5));
        let e95  = channel_sum(&render(&spec(), &fans(), 0.95));
        assert!(
            e95 < e50,
            "fans: energy at t=0.95 ({e95}) must be < energy at t=0.5 ({e50})"
        );
    }

    #[test]
    fn energy_dieout_strip() {
        let e50  = channel_sum(&render(&spec(), &strip(),  0.5));
        let e95  = channel_sum(&render(&spec(), &strip(), 0.95));
        assert!(
            e95 < e50,
            "strip: energy at t=0.95 ({e95}) must be < energy at t=0.5 ({e50})"
        );
    }

    // ---- period boundary t=0.0: origin bright, LEDs at dist>0.25 black ----
    //
    // At t=0: r=0, fade=(1−0)=1.
    //   brightness(dist) = exp(−dist²/(2·0.06²)) = exp(−dist²/0.0072).
    //
    // Origin:
    //   Fans LED 0 (dist=0):   exp(0)·1 = 1.0 → channel 255.
    //   Strip LED 66 (dist=0): same → channel 255.
    //
    // Beyond dist=0.25 (effectively black):
    //   exp(−0.25²/0.0072) = exp(−8.68) ≈ 0.000169 → 255·0.000169 ≈ 0.043 → rounds to 0.
    //   Fan LED 11 (dist=11/44=0.25):   channel = round(255·0.000169) = 0.
    //   Fan LED 12 (dist=12/44≈0.273):  even smaller → 0.
    //
    // For Strip: dist=0.25 → |pos−0.5|=0.125 → pos=0.375 or 0.625 → i≈49.5 or 82.5.
    //   LEDs 0..49 (outer, dist≥0.258) and LEDs 83..131 all round to 0.

    #[test]
    fn boundary_t0_fans() {
        let frame = render(&spec(), &fans(), 0.0);
        let lf = 44usize;

        // Origin (LED 0 of each fan, dist=0) must be bright (channel = 255)
        for fan in 0..3usize {
            let origin = frame[fan * lf];
            assert_eq!(
                origin, [255, 255, 255],
                "fan {fan} LED 0 (dist=0, t=0) must be [255,255,255], got {origin:?}"
            );
        }

        // LEDs with dist > 0.25 must round to black (channel = 0).
        // i in 11..34 (exclusive) → dist = min(i/44, 1−i/44) ∈ [0.25, 0.5].
        // (i=11 → dist=11/44=0.25; i=22 → dist=0.5; i=33 → dist=11/44=0.25)
        // All round to 0 as shown above.
        for fan in 0..3usize {
            for i in 11..34usize {
                let idx = fan * lf + i;
                let c = frame[idx];
                assert!(
                    c == [0, 0, 0],
                    "fan {fan} LED {i} (dist≥0.25, t=0) must be black, got {c:?}"
                );
            }
        }
    }

    #[test]
    fn boundary_t0_strip() {
        let frame = render(&spec(), &strip(), 0.0);
        let total = 132usize;

        // Center (LED 66, pos=0.5, dist=0) must be bright
        let center = frame[total / 2]; // LED 66
        assert_eq!(
            center, [255, 255, 255],
            "strip center LED 66 (dist=0, t=0) must be [255,255,255], got {center:?}"
        );

        // Outer LEDs with dist > 0.25 must be black.
        // dist = |pos−0.5|·2 > 0.25 → pos < 0.375 or pos > 0.625
        // → i < 49.5 → LEDs 0..49; i > 82.5 → LEDs 83..131.
        // Test representative ranges: LEDs 0..49 and 83..131.
        for (i, &c) in frame.iter().enumerate().take(50) {
            assert!(
                c == [0, 0, 0],
                "strip LED {i} (outer, dist>0.25, t=0) must be black, got {c:?}"
            );
        }
        for (i, &c) in frame.iter().enumerate().skip(83) {
            assert!(
                c == [0, 0, 0],
                "strip LED {i} (outer, dist>0.25, t=0) must be black, got {c:?}"
            );
        }
    }

    // =========================================================================
    // Ripple on SlInf44 — tests added in commit 2 (2026-07-14)
    // =========================================================================
    //
    // SlInf44 radial ripple: dist = physical_radius / 1.15 (normalised to 0..=1).
    //   inner ring: radius 0.4 → dist ≈ 0.348
    //   arcs:       radius 1.0 → dist ≈ 0.870
    //   strips:     radius 1.15 → dist = 1.0
    //
    // Wavefront r = t; brightness = exp(−(dist − r)² / TWO_SIGMA_SQ) × (1 − t).
    // σ = 0.06, TWO_SIGMA_SQ = 0.0072.

    fn sl_inf44_fans_ripple() -> Geometry {
        Geometry::Fans {
            fan_count: 1,
            leds_per_fan: 44,
            layout: crate::geometry::FanLayout::SlInf44,
        }
    }

    fn max_channel_slice(frame: &[[u8; 3]], range: std::ops::Range<usize>) -> u8 {
        frame[range].iter().flat_map(|c| c.iter().copied()).max().unwrap_or(0)
    }

    // ---- at t=0.35: inner ring (dist≈0.348) is near the wavefront and bright;
    //      arcs (dist≈0.870) are effectively black ----
    //
    // inner ring: dist ≈ 0.4/1.15 = 0.34783, r = 0.35
    //   (dist−r)² = (−0.00217)² ≈ 4.71e-6 → exp(−4.71e-6/0.0072) ≈ 0.9993
    //   fade = 0.65 → brightness ≈ 0.6495 → channel ≈ 166
    //
    // arcs: dist ≈ 1.0/1.15 = 0.8696, r = 0.35
    //   (dist−r)² ≈ 0.2700 → exp(−37.5) ≈ 0 → channel 0

    #[test]
    fn t035_slinf44_inner_bright_arcs_black() {
        let frame = render(&spec(), &sl_inf44_fans_ripple(), 0.35);

        // Inner ring: indices 0-7. All should be bright.
        let inner_peak = max_channel_slice(&frame, 0..8);
        assert!(
            inner_peak > 100,
            "inner ring at t=0.35 (dist≈0.348≈r) must be bright, got peak={inner_peak}"
        );

        // Left arcs: indices 8-17. Should be near-black (dist 0.870, far from r=0.35).
        let arc_peak = max_channel_slice(&frame, 8..18);
        assert!(
            arc_peak <= 4,
            "left arc at t=0.35 (dist≈0.870) must be near-black, got peak={arc_peak}"
        );

        // Right arcs: indices 26-35. Same reasoning.
        let rarc_peak = max_channel_slice(&frame, 26..36);
        assert!(
            rarc_peak <= 4,
            "right arc at t=0.35 must be near-black, got peak={rarc_peak}"
        );
    }

    // ---- at t=0.95: inner ring is black; side strips carry the faded front ----
    //
    // inner ring: dist ≈ 0.348, r = 0.95
    //   (dist−r)² = (−0.602)² = 0.362 → exp(−50.3) ≈ 0 → channel 0
    //
    // strips: dist = 1.0, r = 0.95
    //   (dist−r)² = 0.05² = 0.0025 → exp(−0.347) ≈ 0.707
    //   fade = 0.05 → brightness ≈ 0.0354 → channel ≈ 9

    #[test]
    fn t095_slinf44_inner_black_strips_carry_front() {
        let frame = render(&spec(), &sl_inf44_fans_ripple(), 0.95);

        // Inner ring must be black.
        let inner_peak = max_channel_slice(&frame, 0..8);
        assert!(
            inner_peak <= 4,
            "inner ring at t=0.95 must be near-black (wavefront far past inner), got {inner_peak}"
        );

        // Side strips (indices 18-25 and 36-43) carry the faded front.
        // Expected channel ≈ 9 — must be > 4 (nonzero at the wave).
        let lstrip_peak = max_channel_slice(&frame, 18..26);
        let rstrip_peak = max_channel_slice(&frame, 36..44);
        assert!(
            lstrip_peak > 4,
            "left strip at t=0.95 (dist=1.0, near wavefront) must be nonzero, got {lstrip_peak}"
        );
        assert!(
            rstrip_peak > 4,
            "right strip at t=0.95 must be nonzero, got {rstrip_peak}"
        );
    }

    // ---- fans in-phase: all fan slices are identical at any t ----
    //
    // dist = min(ring_angle(i, lf), 1−ring_angle(i, lf)) depends only on i (not fan).
    // Therefore frame[fan*lf + i] == frame[i] for all fans.

    #[test]
    fn fans_in_phase() {
        // Test at t=0.3 (arbitrary, wavefront mid-travel)
        let frame = render(&spec(), &fans(), 0.3);
        let lf = 44usize;
        let fan0: &[[u8; 3]] = &frame[0..lf];
        let fan1: &[[u8; 3]] = &frame[lf..2*lf];
        let fan2: &[[u8; 3]] = &frame[2*lf..3*lf];
        assert_eq!(fan0, fan1, "fan 0 and fan 1 must be in phase");
        assert_eq!(fan1, fan2, "fan 1 and fan 2 must be in phase");
    }
}
