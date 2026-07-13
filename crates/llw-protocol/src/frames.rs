//! Pure builders for the 240-byte RF frames and their 64-byte USB chunks.
//! No I/O here — byte-exact and fully unit-tested.

use crate::consts::*;
use crate::device_kind::DeviceKind;

pub type RfFrame = [u8; RF_DATA_SIZE];

/// Maximum compressed payload `rgb_frames` can address: the packet-count
/// byte [19] is a u8 counting data packets + 1 header, so at most 254
/// data packets of RGB_CHUNK_LEN bytes each.
pub const RGB_MAX_COMPRESSED: usize = 254 * RGB_CHUNK_LEN;

/// Split a 240-byte RF frame into 4× 64-byte USB packets for the TX dongle.
/// Packet layout: [0]=0x10, [1]=chunk index, [2]=channel, [3]=rx_type, [4..64]=60-byte chunk.
pub fn usb_chunks(rf_data: &RfFrame, channel: u8, rx_type: u8) -> [[u8; 64]; RF_CHUNKS] {
    let mut packets = [[0u8; 64]; RF_CHUNKS];
    for (chunk_idx, packet) in packets.iter_mut().enumerate() {
        packet[0] = USB_CMD_SEND_RF;
        packet[1] = chunk_idx as u8;
        packet[2] = channel;
        packet[3] = rx_type;
        let start = chunk_idx * RF_CHUNK_SIZE;
        packet[4..64].copy_from_slice(&rf_data[start..start + RF_CHUNK_SIZE]);
    }
    packets
}

/// Build a PWM command frame.
/// Layout: [0]=0x12, [1]=0x10, [2..8]=device MAC, [8..14]=master MAC,
/// [14]=rx_type, [15]=master channel, [16]=sequence index, [17..21]=PWM×4.
pub fn pwm_frame(
    device_mac: &[u8; 6],
    master_mac: &[u8; 6],
    rx_type: u8,
    master_channel: u8,
    seq_index: u8,
    pwm: &[u8; 4],
) -> RfFrame {
    let mut rf = [0u8; RF_DATA_SIZE];
    rf[0] = RF_SELECT;
    rf[1] = RF_PWM_CMD;
    rf[2..8].copy_from_slice(device_mac);
    rf[8..14].copy_from_slice(master_mac);
    rf[14] = rx_type;
    rf[15] = master_channel;
    rf[16] = seq_index;
    rf[17..21].copy_from_slice(pwm);
    rf
}

/// Build the 1Hz master-clock heartbeat frame (broadcast, rx_type 0xFF at the
/// USB layer). The 220-byte cpu-info field is left zero — firmware only needs
/// the heartbeat itself.
pub fn master_clock_frame(master_mac: &[u8; 6]) -> RfFrame {
    let mut rf = [0u8; RF_DATA_SIZE];
    rf[0] = RF_SELECT;
    rf[1] = RF_MASTER_CLOCK;
    rf[8..14].copy_from_slice(master_mac);
    rf
}

/// Build the RGB upload frame sequence for a compressed payload.
/// Returns the header frame first (callers re-send it several times for RF
/// reliability), then the
/// data frames carrying 220-byte chunks of `compressed`.
///
/// Header ([18]=0): [20..24]=compressed len u32 BE, [25..27]=frame count u16 BE,
/// [27]=LED count, [32..34]=interval ms u16 BE.
/// Data ([18]=n): [20..20+chunk]=compressed bytes.
/// All frames: [14..18]=effect index, [19]=total packet count (data pkts + 1).
pub fn rgb_frames(
    device_mac: &[u8; 6],
    master_mac: &[u8; 6],
    effect_index: &[u8; 4],
    compressed: &[u8],
    led_num: u8,
    total_frames: u16,
    interval_ms: u16,
) -> Vec<RfFrame> {
    assert!(
        compressed.len() <= RGB_MAX_COMPRESSED,
        "compressed RGB payload {} B exceeds protocol max {} B (254 packets)",
        compressed.len(),
        RGB_MAX_COMPRESSED
    );
    let total_pk_num = compressed.len().div_ceil(RGB_CHUNK_LEN) as u8;
    let mut frames = Vec::with_capacity(total_pk_num as usize + 1);

    let mut base = [0u8; RF_DATA_SIZE];
    base[0] = RF_SELECT;
    base[1] = RF_SET_RGB;
    base[2..8].copy_from_slice(device_mac);
    base[8..14].copy_from_slice(master_mac);
    base[14..18].copy_from_slice(effect_index);
    base[19] = total_pk_num + 1;

    // Header frame (index 0)
    let mut header = base;
    header[18] = 0;
    let data_len = compressed.len() as u32;
    header[20..24].copy_from_slice(&data_len.to_be_bytes());
    header[24] = 0;
    header[25..27].copy_from_slice(&total_frames.to_be_bytes());
    header[27] = led_num;
    header[32..34].copy_from_slice(&interval_ms.to_be_bytes());
    frames.push(header);

    // Data frames (index 1..)
    for (i, chunk) in compressed.chunks(RGB_CHUNK_LEN).enumerate() {
        let mut data = base;
        data[18] = (i + 1) as u8;
        data[20..20 + chunk.len()].copy_from_slice(chunk);
        frames.push(data);
    }

    frames
}

/// Clamp PWM targets to hardware constraints:
/// - slots beyond `fan_count` are zeroed (except the AIO pump slot 3)
/// - nonzero values below the kind's minimum duty are raised to it
/// - CLV1 firmware quirk: 153/154 → 152, 155 → 156
pub fn apply_pwm_constraints(pwm: &mut [u8; 4], kind: DeviceKind, fan_count: u8) {
    let min_pwm = ((kind.min_duty_percent() as f32 / 100.0) * 255.0) as u8;

    for (i, val) in pwm.iter_mut().enumerate() {
        let is_pump_slot = i == 3 && kind.is_aio();
        if i as u8 >= fan_count && !is_pump_slot {
            *val = 0;
            continue;
        }
        if *val > 0 && *val < min_pwm {
            *val = min_pwm;
        }
        if kind == DeviceKind::Clv1 {
            match *val {
                153 | 154 => *val = 152,
                155 => *val = 156,
                _ => {}
            }
        }
    }
}

/// FNV-1a hash of an LED state, used as the RGB effect index. The firmware
/// echoes it back in device records; a mismatch means the firmware reset its
/// lighting (idle watchdog) and the RGB should be re-sent. Never returns
/// all-zero (0 is mapped to 1).
pub fn effect_index_from_leds(leds: &[[u8; 3]]) -> [u8; 4] {
    let mut h: u32 = 0x811c_9dc5;
    for px in leds {
        for &b in px {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    if h == 0 {
        h = 1;
    }
    h.to_be_bytes()
}

/// FNV-1a hash across ALL frames of an animation (matches upstream's
/// `effect_index_from_frames`; byte-identical to hashing the concatenated
/// frame data). For a single frame this equals `effect_index_from_leds`.
pub fn effect_index_from_frames(frames: &[Vec<[u8; 3]>]) -> [u8; 4] {
    let mut h: u32 = 0x811c_9dc5;
    for frame in frames {
        for px in frame {
            for &b in px {
                h ^= b as u32;
                h = h.wrapping_mul(0x0100_0193);
            }
        }
    }
    if h == 0 {
        h = 1;
    }
    h.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    const MASTER: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    const FX: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

    #[test]
    fn usb_chunking_is_byte_exact() {
        let mut rf = [0u8; RF_DATA_SIZE];
        for (i, b) in rf.iter_mut().enumerate() {
            *b = i as u8;
        }
        let packets = usb_chunks(&rf, 5, 3);
        assert_eq!(packets.len(), 4);
        for (idx, p) in packets.iter().enumerate() {
            assert_eq!(p[0], 0x10);
            assert_eq!(p[1], idx as u8);
            assert_eq!(p[2], 5);
            assert_eq!(p[3], 3);
            assert_eq!(&p[4..64], &rf[idx * 60..idx * 60 + 60]);
        }
    }

    #[test]
    fn pwm_frame_layout() {
        let rf = pwm_frame(&MAC, &MASTER, 3, 2, 1, &[100, 100, 0, 0]);
        assert_eq!(rf[0], 0x12);
        assert_eq!(rf[1], 0x10);
        assert_eq!(&rf[2..8], &MAC);
        assert_eq!(&rf[8..14], &MASTER);
        assert_eq!(rf[14], 3); // rx_type
        assert_eq!(rf[15], 2); // master channel
        assert_eq!(rf[16], 1); // seq
        assert_eq!(&rf[17..21], &[100, 100, 0, 0]);
        assert_eq!(&rf[21..], &[0u8; 219][..]); // padding untouched
    }

    #[test]
    fn master_clock_frame_layout() {
        let rf = master_clock_frame(&MASTER);
        assert_eq!(rf[0], 0x12);
        assert_eq!(rf[1], 0x14);
        assert_eq!(&rf[2..8], &[0u8; 6][..]); // no device MAC (broadcast)
        assert_eq!(&rf[8..14], &MASTER);
        assert_eq!(&rf[14..], &[0u8; 226][..]);
    }

    #[test]
    fn rgb_frames_header_and_chunking() {
        // 250 compressed bytes → 1 header + 2 data frames (220 + 30)
        let compressed = vec![0xAB; 250];
        let frames = rgb_frames(&MAC, &MASTER, &FX, &compressed, 44, 1, 5000);
        assert_eq!(frames.len(), 3);

        let h = &frames[0];
        assert_eq!(h[0], 0x12);
        assert_eq!(h[1], 0x20);
        assert_eq!(&h[2..8], &MAC);
        assert_eq!(&h[8..14], &MASTER);
        assert_eq!(&h[14..18], &FX);
        assert_eq!(h[18], 0); // header index
        assert_eq!(h[19], 3); // total packets incl. header
        assert_eq!(&h[20..24], &250u32.to_be_bytes());
        assert_eq!(h[24], 0);
        assert_eq!(&h[25..27], &1u16.to_be_bytes());
        assert_eq!(h[27], 44); // led count
        assert_eq!(&h[32..34], &5000u16.to_be_bytes());
        assert_eq!(&h[28..32], &[0u8; 4][..]); // reserved, must stay zero
        assert_eq!(&h[34..], &[0u8; 206][..]); // header padding

        let d1 = &frames[1];
        assert_eq!(d1[18], 1);
        assert_eq!(d1[19], 3);
        assert_eq!(&d1[20..240], &compressed[0..220]);

        let d2 = &frames[2];
        assert_eq!(d2[18], 2);
        assert_eq!(&d2[20..50], &compressed[220..250]);
        assert_eq!(&d2[50..], &[0u8; 190][..]); // rest zero
    }

    #[test]
    fn rgb_frames_exact_chunk_boundary() {
        // exactly 220 bytes → 1 header + 1 data frame
        let compressed = vec![0x01; 220];
        let frames = rgb_frames(&MAC, &MASTER, &FX, &compressed, 88, 4, 100);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0][19], 2);
        assert_eq!(frames[0][27], 88);
        assert_eq!(&frames[0][25..27], &4u16.to_be_bytes());
        assert_eq!(&frames[0][32..34], &100u16.to_be_bytes());
    }

    #[test]
    #[should_panic(expected = "exceeds protocol max")]
    fn rgb_frames_rejects_oversized_payload() {
        let compressed = vec![0u8; RGB_MAX_COMPRESSED + 1];
        let _ = rgb_frames(&MAC, &MASTER, &FX, &compressed, 44, 1, 100);
    }

    #[test]
    fn pwm_constraints() {
        use crate::device_kind::DeviceKind;

        // SL-INF: min duty 11% → floor((0.11)*255) = 28
        let mut pwm = [5, 100, 40, 40];
        apply_pwm_constraints(&mut pwm, DeviceKind::SlInf, 2);
        assert_eq!(pwm, [28, 100, 0, 0]); // slot0 raised, slots ≥ fan_count zeroed

        // zero stays zero (fan off is allowed)
        let mut pwm = [0, 100, 0, 0];
        apply_pwm_constraints(&mut pwm, DeviceKind::SlInf, 2);
        assert_eq!(pwm, [0, 100, 0, 0]);

        // CLV1 quirk filter
        let mut pwm = [153, 154, 155, 156];
        apply_pwm_constraints(&mut pwm, DeviceKind::Clv1, 4);
        assert_eq!(pwm, [152, 152, 156, 156]);

        // AIO pump slot survives fan_count
        let mut pwm = [100, 0, 0, 200];
        apply_pwm_constraints(&mut pwm, DeviceKind::WaterBlock, 1);
        assert_eq!(pwm, [100, 0, 0, 200]);
    }

    #[test]
    fn effect_index_fnv1a() {
        // FNV-1a 32-bit of "abc" (0x61 0x62 0x63) is the well-known 0x1a47e90b.
        // If this assertion fails, verify independently:
        //   python3 -c "h=0x811c9dc5
        //   for b in b'abc': h=((h^b)*0x01000193)&0xFFFFFFFF
        //   print(hex(h))"
        assert_eq!(
            effect_index_from_leds(&[[0x61, 0x62, 0x63]]),
            0x1a47e90bu32.to_be_bytes()
        );
        // deterministic + input-sensitive + never zero
        let a = effect_index_from_leds(&[[255, 0, 0]; 44]);
        let b = effect_index_from_leds(&[[255, 0, 0]; 44]);
        let c = effect_index_from_leds(&[[0, 255, 0]; 44]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(effect_index_from_leds(&[]), [0, 0, 0, 0]);
    }

    #[test]
    fn effect_index_from_frames_covers_all_frames() {
        let f0 = vec![[255, 0, 0]; 4];
        let a = effect_index_from_frames(&[f0.clone(), vec![[0, 255, 0]; 4]]);
        let b = effect_index_from_frames(&[f0.clone(), vec![[0, 0, 255]; 4]]);
        assert_ne!(a, b); // animations sharing frame 0 must differ
        assert_eq!(effect_index_from_frames(&[f0.clone()]), effect_index_from_leds(&f0));
    }
}
