//! Bridge between daemon protocol types and the pure `llw-effects` engine.
//!
//! This module is the only place in the daemon that knows both `DeviceKind`
//! and `Geometry` — keeping `llw-effects` dependency-free of protocol types.

use llw_effects::{render_animation, EffectSpec, Geometry};
use llw_effects::geometry::FanLayout;
use llw_protocol::device_kind::DeviceKind;
use llw_protocol::record::DeviceRecord;

/// Raw-byte budget for animation storage, derived from the Task 8 flash probe.
///
/// Probe data (2026-07-14, SL-INF @ 132 LEDs):
///   - 96 frames × 132 LEDs × 3 bytes = 38,016 raw bytes → PASS
///   - 112 frames × 132 LEDs × 3 bytes = 44,352 raw bytes → FAIL (firmware wipes fx)
///
/// Measured floor ≈ 38 KB; this budget is ~75% of the floor (conservative).
/// Frame count = clamp(RAW_BYTE_BUDGET / (total_leds × 3), 8, 96).
const RAW_BYTE_BUDGET: u32 = 28_000;

/// Compute the frame budget for a device with `total_leds` LEDs.
///
/// Formula: `clamp(RAW_BYTE_BUDGET / (total_leds × 3), 8, 96)`.
///
/// Probe anchor points (Task 8, 2026-07-14):
/// - 132 LEDs → 28_000 / 396 = 70 frames
/// - 174 LEDs → 28_000 / 522 = 53 frames
/// - 44 LEDs  → 212 → clamped to 96 frames
/// - 4000 LEDs → 2 → clamped to 8 frames
pub fn frame_budget(total_leds: u16) -> u16 {
    if total_leds == 0 {
        return 8;
    }
    let raw = RAW_BYTE_BUDGET / (total_leds as u32 * 3);
    raw.clamp(8, 96) as u16
}

/// Map a device kind + fan_count to an `llw-effects` Geometry, if supported.
///
/// - Fan devices → `Geometry::Fans { fan_count, leds_per_fan }` (leds_per_fan from kind).
/// - Flat-buffer devices (`led_count_override` is Some) → `Geometry::Strip { total }`.
/// - AIO (pump + fan composite) → `None`.
///   Post-v1: pump+fans composite geometry; static color still works for AIOs.
/// - Unknown or zero leds_per_fan for a fan device with no override → `None`.
pub fn geometry_of(kind: DeviceKind, fan_count: u8) -> Option<Geometry> {
    // Post-v1: pump+fans composite geometry for AIOs.
    if kind.is_aio() {
        return None;
    }

    // Flat-buffer devices (Strimer, Lc217, Led88, V150) use Strip geometry.
    if let Some(total) = kind.led_count_override() {
        return Some(Geometry::Strip { total });
    }

    // Fan devices: leds_per_fan must be nonzero.
    let lpf = kind.leds_per_fan();
    if lpf == 0 || fan_count == 0 {
        return None;
    }

    // Use the empirically measured SL-INF layout; all other fan kinds remain
    // UniformRing until their wiring is chase-probed in a future session.
    let layout = match kind {
        DeviceKind::SlInf => FanLayout::SlInf44,
        _ => FanLayout::UniformRing,
    };

    Some(Geometry::Fans { fan_count, leds_per_fan: lpf, layout })
}

/// Compile an `EffectSpec` against a `DeviceRecord` into animation frames.
///
/// Returns `(frames, interval_ms)` on success, or `None` if:
/// - the device geometry is unsupported (AIO, zero-LED fan, unknown kind), or
/// - the rendered frame length does not match `rec.total_leds()` (geometry/record
///   mismatch — better to skip the upload than send garbage to the firmware).
///
/// `frame_budget` is the number of frames to render. Callers may pass any value;
/// the supervisor uses [`frame_budget`] to compute a data-driven default. Test
/// callers pass an explicit count (e.g. 24) to keep golden-frame expectations stable.
pub fn compile(
    spec: &EffectSpec,
    rec: &DeviceRecord,
    frame_budget: u16,
) -> Option<(Vec<Vec<[u8; 3]>>, u16)> {
    let geom = geometry_of(rec.kind, rec.fan_count)?;

    let (frames, interval_ms) = render_animation(spec, &geom, frame_budget);

    // Sanity: every frame must have exactly total_leds() pixels.
    let expected_leds = rec.total_leds() as usize;
    for frame in &frames {
        debug_assert_eq!(
            frame.len(),
            expected_leds,
            "geometry_of and total_leds disagree: frame has {} LEDs, record says {}",
            frame.len(),
            expected_leds
        );
        if frame.len() != expected_leds {
            // In release builds a mismatch means geometry_of and total_leds are
            // inconsistent — skip the upload rather than send malformed data.
            return None;
        }
    }

    Some((frames, interval_ms))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use llw_effects::{Direction, EffectKind, EffectSpec};
    use llw_protocol::record::parse_device_record;

    fn make_raw_record(device_type: u8, fan_count: u8, fan_type_byte: u8) -> [u8; 42] {
        let mut r = [0u8; 42];
        // MAC: 02:8b:51:62:32:e1
        r[0..6].copy_from_slice(&[0x02, 0x8b, 0x51, 0x62, 0x32, 0xe1]);
        // master: e5:ba:f0:72:ab:3c
        r[6..12].copy_from_slice(&[0xe5, 0xba, 0xf0, 0x72, 0xab, 0x3c]);
        r[12] = 2; // channel
        r[13] = 1; // rx_type
        r[18] = device_type;
        r[19] = fan_count;
        r[24] = fan_type_byte;
        r[41] = 0x1C; // marker
        r
    }

    fn sl_inf_record(fan_count: u8) -> DeviceRecord {
        // SL-INF: device_type 0 (fan device), fan type byte 36 (SlInf), 44 LEDs/fan
        let raw = make_raw_record(0, fan_count, 36);
        parse_device_record(&raw, 0).expect("valid sl-inf record")
    }

    fn strimer2_record() -> DeviceRecord {
        // Strimer(2): device_type 2, no fans, 132 LEDs strip
        let raw = make_raw_record(2, 0, 0);
        parse_device_record(&raw, 0).expect("valid strimer record")
    }

    fn aio_record() -> DeviceRecord {
        // WaterBlock: device_type 10, 1 fan head, AIO
        let raw = make_raw_record(10, 1, 0);
        parse_device_record(&raw, 0).expect("valid aio record")
    }

    fn ripple_spec() -> EffectSpec {
        EffectSpec {
            kind: EffectKind::Ripple,
            colors: vec![[0, 0, 255], [136, 0, 255]],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    // --- geometry_of ---

    #[test]
    fn sl_inf_3fan_geometry() {
        let geom = geometry_of(DeviceKind::SlInf, 3).expect("SL-INF should have geometry");
        assert_eq!(geom, Geometry::Fans { fan_count: 3, leds_per_fan: 44, layout: FanLayout::SlInf44 });
        assert_eq!(geom.len(), 132);
    }

    #[test]
    fn strimer2_geometry() {
        let geom = geometry_of(DeviceKind::Strimer(2), 0).expect("Strimer(2) should have geometry");
        assert_eq!(geom, Geometry::Strip { total: 132 });
        assert_eq!(geom.len(), 132);
    }

    #[test]
    fn aio_geometry_is_none() {
        assert!(
            geometry_of(DeviceKind::WaterBlock, 1).is_none(),
            "AIO geometry must be None (post-v1)"
        );
        assert!(
            geometry_of(DeviceKind::WaterBlock2, 2).is_none(),
            "AIO geometry must be None (post-v1)"
        );
    }

    // --- compile ---

    #[test]
    fn sl_inf_3fan_compile_24_frames_132_leds() {
        let rec = sl_inf_record(3);
        let spec = ripple_spec();
        let (frames, interval_ms) = compile(&spec, &rec, 24).expect("SL-INF compile must succeed");
        assert_eq!(frames.len(), 24, "should render 24 frames");
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(frame.len(), 132, "frame {i} must have 132 LEDs (3×44)");
        }
        // interval = period_ms(3) / 24 = 3000 / 24 = 125ms
        assert_eq!(interval_ms, 125, "interval should be 125ms for 24 frames at speed 3");
    }

    #[test]
    fn strimer2_compile_strip_132_leds() {
        let rec = strimer2_record();
        let spec = ripple_spec();
        let (frames, _interval_ms) =
            compile(&spec, &rec, 24).expect("Strimer(2) compile must succeed");
        assert_eq!(frames.len(), 24);
        for frame in &frames {
            assert_eq!(frame.len(), 132, "Strimer(2) frame must have 132 LEDs");
        }
    }

    #[test]
    fn aio_compile_returns_none() {
        let rec = aio_record();
        let spec = ripple_spec();
        assert!(
            compile(&spec, &rec, 24).is_none(),
            "AIO compile must return None"
        );
    }

    #[test]
    fn frame_led_count_consistency() {
        // geometry_of and total_leds must agree for all supported device kinds.
        let rec = sl_inf_record(3);
        let (frames, _) = compile(&ripple_spec(), &rec, 8).unwrap();
        // Every frame must match rec.total_leds().
        let expected = rec.total_leds() as usize;
        assert!(frames.iter().all(|f| f.len() == expected));
    }

    // --- frame_budget ---
    //
    // Formula: clamp(28_000 / (total_leds × 3), 8, 96).

    #[test]
    fn frame_budget_132_leds() {
        // 28_000 / (132 × 3) = 28_000 / 396 = 70 (truncated)
        assert_eq!(super::frame_budget(132), 70);
    }

    #[test]
    fn frame_budget_174_leds() {
        // 28_000 / (174 × 3) = 28_000 / 522 = 53 (truncated)
        assert_eq!(super::frame_budget(174), 53);
    }

    #[test]
    fn frame_budget_44_leds_capped() {
        // 28_000 / (44 × 3) = 28_000 / 132 = 212 → clamped to 96
        assert_eq!(super::frame_budget(44), 96);
    }

    #[test]
    fn frame_budget_4000_leds_floored() {
        // 28_000 / (4000 × 3) = 28_000 / 12_000 = 2 → clamped to 8
        assert_eq!(super::frame_budget(4000), 8);
    }
}
