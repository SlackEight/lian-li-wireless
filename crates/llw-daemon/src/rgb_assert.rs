//! Static-color assertion (pure): config → full-device LED frame, expected
//! effect index, and drift comparison. M3 replaces the frame source with the
//! effect engine; the drift plumbing stays identical.

use crate::config::StaticColor;
use llw_protocol::frames::effect_index_from_frames;
use llw_protocol::record::DeviceRecord;

/// Upstream brightness math: channel × (brightness / 4).clamp(0, 1).
pub fn scaled_color(c: [u8; 3], brightness: u8) -> [u8; 3] {
    let k = (brightness as f32 / 4.0).clamp(0.0, 1.0);
    [
        (c[0] as f32 * k) as u8,
        (c[1] as f32 * k) as u8,
        (c[2] as f32 * k) as u8,
    ]
}

/// The full-device single frame for a static color.
pub fn static_frame(rec: &DeviceRecord, color: &StaticColor) -> Vec<[u8; 3]> {
    vec![scaled_color(color.rgb, color.brightness); rec.total_leds() as usize]
}

/// The effect index `Dongle::upload_rgb` will produce for this frame —
/// compare against the device record's echoed index to detect firmware drift.
pub fn expected_index(frame: &[[u8; 3]]) -> [u8; 4] {
    effect_index_from_frames(std::slice::from_ref(&frame.to_vec()))
}

pub fn drifted(expected: &[u8; 4], reported: &[u8; 4]) -> bool {
    expected != reported
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StaticColor;
    use llw_protocol::record::parse_device_record;

    fn sl_inf_record() -> DeviceRecord {
        // 42-byte synthetic SL-INF record (3 fans × 44 LEDs = 132).
        let mut raw = [0u8; 42];
        raw[18] = 0; // fan device
        raw[19] = 3; // fan count
        raw[24] = 36; // SL-INF fan type byte
        raw[41] = 0x1C;
        parse_device_record(&raw, 0).expect("valid")
    }

    #[test]
    fn brightness_scaling() {
        assert_eq!(scaled_color([255, 255, 255], 4), [255, 255, 255]);
        assert_eq!(scaled_color([255, 255, 255], 2), [127, 127, 127]);
        assert_eq!(scaled_color([255, 100, 0], 0), [0, 0, 0]);
    }

    #[test]
    fn frame_covers_all_leds() {
        let rec = sl_inf_record();
        let frame = static_frame(&rec, &StaticColor { rgb: [255, 0, 0], brightness: 4 });
        assert_eq!(frame.len(), 132);
        assert!(frame.iter().all(|px| *px == [255, 0, 0]));
    }

    #[test]
    fn expected_index_matches_upload_semantics_and_detects_drift() {
        let rec = sl_inf_record();
        let frame = static_frame(&rec, &StaticColor { rgb: [255, 0, 0], brightness: 4 });
        let idx = expected_index(&frame);
        assert_ne!(idx, [0, 0, 0, 0]);
        assert!(!drifted(&idx, &idx));
        // firmware reset to its default index → drift detected
        assert!(drifted(&idx, &[0xd9, 0x2c, 0xb8, 0x51]));
        // different color → different index
        let other = static_frame(&rec, &StaticColor { rgb: [0, 0, 255], brightness: 4 });
        assert!(drifted(&expected_index(&other), &idx));
    }
}
