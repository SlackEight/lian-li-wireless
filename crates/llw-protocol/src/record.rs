//! Parsing of GetDev responses: the RX dongle reports all wireless devices
//! on air as 42-byte records.

use crate::device_kind::DeviceKind;
use tracing::debug;

/// A wireless device as reported by the RX GetDev poll.
///
/// 42-byte record layout:
/// ```text
/// [0-5]   Device MAC        [6-11]  Master MAC       [12] RF channel
/// [13]    RX type           [14-17] System time      [18] Device type
/// [19]    Fan count         [20-23] Effect index     [24-27] Fan-type bytes
/// [27]    Coolant temp °C (AIOs only — overlaps 4th fan-type byte)
/// [28-35] Fan RPM (4× u16 BE)   [36-39] Current PWM (4× u8)
/// [40]    Cmd sequence      [41]    Validation marker (0x1C)
/// ```
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub mac: [u8; 6],
    pub master_mac: [u8; 6],
    pub channel: u8,
    pub rx_type: u8,
    pub device_type: u8,
    pub fan_count: u8,
    pub fan_types: [u8; 4],
    pub fan_rpms: [u16; 4],
    pub current_pwm: [u8; 4],
    pub cmd_seq: u8,
    pub kind: DeviceKind,
    pub list_index: u8,
    /// Coolant temperature in °C. `None` if the device is not an AIO,
    /// or if the sensor byte is 0x00 (sensor absent / not initialized).
    pub coolant_temp_c: Option<u8>,
    /// Effect index the firmware is currently running (drifts on firmware
    /// idle-reset; compare against desired to detect and re-send RGB).
    pub effect_index: [u8; 4],
}

impl DeviceRecord {
    pub fn mac_str(&self) -> String {
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.mac[0], self.mac[1], self.mac[2], self.mac[3], self.mac[4], self.mac[5],
        )
    }

    /// Total LEDs on this device (flat-buffer override, or fans × per-fan + pump).
    pub fn total_leds(&self) -> u16 {
        if let Some(n) = self.kind.led_count_override() {
            return n;
        }
        self.kind.pump_led_count() as u16
            + self.fan_count as u16 * self.kind.leds_per_fan() as u16
    }
}

/// Parse one 42-byte device record. Returns None for invalid records and for
/// the master's own record (device_type 0xFF).
pub fn parse_device_record(data: &[u8], list_index: u8) -> Option<DeviceRecord> {
    if data.len() < 42 {
        return None;
    }
    if data[41] != 0x1C {
        debug!(
            "device record {list_index}: invalid marker 0x{:02x} (expected 0x1c)",
            data[41]
        );
        return None;
    }

    let device_type = data[18];
    if device_type == 0xFF {
        return None; // master's own record
    }

    let mut mac = [0u8; 6];
    mac.copy_from_slice(&data[0..6]);
    let mut master_mac = [0u8; 6];
    master_mac.copy_from_slice(&data[6..12]);

    let channel = data[12];
    let rx_type = data[13];
    let fan_count = data[19].min(4);

    let mut fan_types = [0u8; 4];
    fan_types.copy_from_slice(&data[24..28]);

    let fan_rpms = [
        u16::from_be_bytes([data[28], data[29]]),
        u16::from_be_bytes([data[30], data[31]]),
        u16::from_be_bytes([data[32], data[33]]),
        u16::from_be_bytes([data[34], data[35]]),
    ];

    let mut current_pwm = [0u8; 4];
    current_pwm.copy_from_slice(&data[36..40]);

    let cmd_seq = data[40];

    let kind = match device_type {
        10 => DeviceKind::WaterBlock,
        11 => DeviceKind::WaterBlock2,
        1..=9 => DeviceKind::Strimer(device_type),
        65 => DeviceKind::Lc217,
        66 => DeviceKind::V150,
        88 => DeviceKind::Led88,
        _ => fan_types
            .iter()
            .find(|&&b| b != 0)
            .map(|&b| DeviceKind::from_fan_type_byte(b))
            .unwrap_or(DeviceKind::Unknown),
    };

    let coolant_temp_c = if kind.is_aio() && data[27] > 0 {
        Some(data[27])
    } else {
        None
    };

    let mut effect_index = [0u8; 4];
    effect_index.copy_from_slice(&data[20..24]);

    Some(DeviceRecord {
        mac,
        master_mac,
        channel,
        rx_type,
        device_type,
        fan_count,
        fan_types,
        fan_rpms,
        current_pwm,
        cmd_seq,
        kind,
        list_index,
        coolant_temp_c,
        effect_index,
    })
}

/// Parsed GetDev response.
#[derive(Debug, Default)]
pub struct GetDevReport {
    /// Motherboard PWM duty (0-255) as measured by the master, if available.
    pub mobo_pwm: Option<u8>,
    pub devices: Vec<DeviceRecord>,
}

/// Parse a full GetDev USB response buffer (`len` = bytes actually read).
///
/// Response layout: [0]=0x10 echo, [1]=device_count,
/// [2]=mobo PWM off_time (high bit = unavailable), [3]=mobo PWM on_time,
/// [4..]=42-byte records × device_count.
///
/// Returns None if the response is not a GetDev echo.
pub fn parse_getdev_response(response: &[u8], len: usize) -> Option<GetDevReport> {
    let response = response.get(..len)?;
    if response.len() < 4 || response[0] != crate::consts::USB_CMD_SEND_RF {
        return None;
    }

    let mut report = GetDevReport::default();

    let indicator = response[2];
    if indicator >> 7 == 0 {
        let off_time = (indicator & 0x7F) as u16;
        let on_time = response[3] as u16;
        let denominator = off_time + on_time;
        if denominator > 0 {
            report.mobo_pwm = Some((255u16 * on_time / denominator).min(255) as u8);
        }
    }

    let device_count = response[1] as usize;
    if device_count > 12 {
        debug!("GetDev: implausible device_count {device_count} (max 12), ignoring records");
        return Some(report);
    }
    if device_count == 0 {
        return Some(report);
    }

    let mut offset = 4;
    for idx in 0..device_count {
        if offset + 42 > len {
            debug!("GetDev: response truncated at device {idx}");
            break;
        }
        if let Some(rec) = parse_device_record(&response[offset..offset + 42], idx as u8) {
            report.devices.push(rec);
        }
        offset += 42;
    }

    Some(report)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a synthetic 42-byte record for tests.
    pub(crate) fn make_record(
        mac: [u8; 6],
        master_mac: [u8; 6],
        channel: u8,
        rx_type: u8,
        device_type: u8,
        fan_count: u8,
        fan_types: [u8; 4],
        rpms: [u16; 4],
        pwm: [u8; 4],
    ) -> [u8; 42] {
        let mut r = [0u8; 42];
        r[0..6].copy_from_slice(&mac);
        r[6..12].copy_from_slice(&master_mac);
        r[12] = channel;
        r[13] = rx_type;
        r[18] = device_type;
        r[19] = fan_count;
        r[20..24].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // effect index
        r[24..28].copy_from_slice(&fan_types);
        for (i, rpm) in rpms.iter().enumerate() {
            r[28 + i * 2..30 + i * 2].copy_from_slice(&rpm.to_be_bytes());
        }
        r[36..40].copy_from_slice(&pwm);
        r[40] = 7; // cmd_seq
        r[41] = 0x1C; // marker
        r
    }

    const MAC: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    const MASTER: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

    #[test]
    fn parses_sl_inf_fan_record() {
        let raw = make_record(
            MAC, MASTER, 2, 3, 0, 2, [36, 36, 0, 0],
            [731, 735, 0, 0], [86, 86, 0, 0],
        );
        let rec = parse_device_record(&raw, 0).expect("valid record");
        assert_eq!(rec.mac, MAC);
        assert_eq!(rec.master_mac, MASTER);
        assert_eq!(rec.channel, 2);
        assert_eq!(rec.rx_type, 3);
        assert_eq!(rec.kind, crate::device_kind::DeviceKind::SlInf);
        assert_eq!(rec.fan_count, 2);
        assert_eq!(rec.fan_rpms, [731, 735, 0, 0]);
        assert_eq!(rec.current_pwm, [86, 86, 0, 0]);
        assert_eq!(rec.effect_index, [0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(rec.total_leds(), 88); // 2 fans × 44
        assert_eq!(rec.coolant_temp_c, None);
    }

    #[test]
    fn parses_strimer_record() {
        let raw = make_record(
            MAC, MASTER, 2, 4, 2, 0, [0, 0, 0, 0], [0; 4], [0; 4],
        );
        let rec = parse_device_record(&raw, 1).expect("valid record");
        assert_eq!(rec.kind, crate::device_kind::DeviceKind::Strimer(2));
        assert_eq!(rec.total_leds(), 132);
    }

    #[test]
    fn rejects_bad_marker_and_master_and_truncated() {
        let mut raw = make_record(MAC, MASTER, 2, 3, 0, 2, [36; 4], [0; 4], [0; 4]);
        raw[41] = 0x00;
        assert!(parse_device_record(&raw, 0).is_none());

        let master_rec = make_record(MAC, MASTER, 2, 3, 0xFF, 0, [0; 4], [0; 4], [0; 4]);
        assert!(parse_device_record(&master_rec, 0).is_none());

        assert!(parse_device_record(&[0u8; 30], 0).is_none());
    }

    #[test]
    fn parses_getdev_response_with_mobo_pwm() {
        let rec = make_record(MAC, MASTER, 2, 3, 0, 2, [36, 36, 0, 0], [700; 4], [86; 4]);
        let mut resp = vec![0u8; 4 + 42];
        resp[0] = 0x10;
        resp[1] = 1; // one device
        resp[2] = 3; // off_time 3, high bit clear
        resp[3] = 1; // on_time 1 → pwm = 255*1/4 = 63
        resp[4..46].copy_from_slice(&rec);

        let report = parse_getdev_response(&resp, resp.len()).expect("valid response");
        assert_eq!(report.mobo_pwm, Some(63));
        assert_eq!(report.devices.len(), 1);
        assert_eq!(report.devices[0].mac, MAC);
    }

    #[test]
    fn getdev_mobo_pwm_unavailable_and_wrong_echo() {
        let mut resp = vec![0u8; 4];
        resp[0] = 0x10;
        resp[1] = 0;
        resp[2] = 0x80; // high bit set = unavailable
        resp[3] = 0;
        let report = parse_getdev_response(&resp, 4).expect("valid response");
        assert_eq!(report.mobo_pwm, None);
        assert!(report.devices.is_empty());

        resp[0] = 0x77;
        assert!(parse_getdev_response(&resp, 4).is_none());
    }

    #[test]
    fn getdev_truncated_record_is_skipped() {
        let rec = make_record(MAC, MASTER, 2, 3, 0, 2, [36; 4], [0; 4], [0; 4]);
        let mut resp = vec![0u8; 4 + 42 + 10]; // second record truncated
        resp[0] = 0x10;
        resp[1] = 2;
        resp[2] = 0x80;
        resp[4..46].copy_from_slice(&rec);
        let report = parse_getdev_response(&resp, resp.len()).expect("valid response");
        assert_eq!(report.devices.len(), 1);
    }

    #[test]
    fn parses_aio_coolant_temp() {
        let mut raw = make_record(
            MAC, MASTER, 2, 5, 10, 1, [0, 0, 0, 0], [0, 0, 0, 1900], [0, 0, 0, 128],
        );
        raw[27] = 34; // coolant temp — overlaps 4th fan-type byte
        let rec = parse_device_record(&raw, 0).expect("valid record");
        assert_eq!(rec.kind, crate::device_kind::DeviceKind::WaterBlock);
        assert_eq!(rec.coolant_temp_c, Some(34));
        assert_eq!(rec.total_leds(), 48); // 24 pump + 1 fan × 24

        raw[27] = 0; // sensor byte zero → None even for AIO
        let rec = parse_device_record(&raw, 0).expect("valid record");
        assert_eq!(rec.coolant_temp_c, None);
    }

    #[test]
    fn implausible_device_count_ignored() {
        let mut resp = vec![0u8; 4];
        resp[0] = 0x10;
        resp[1] = 13; // > 12
        resp[2] = 0x80;
        let report = parse_getdev_response(&resp, 4).expect("valid response");
        assert!(report.devices.is_empty());
    }

    #[test]
    fn fan_count_clamped_to_four() {
        let mut raw = make_record(MAC, MASTER, 2, 3, 0, 2, [36, 36, 0, 0], [0; 4], [0; 4]);
        raw[19] = 255;
        let rec = parse_device_record(&raw, 0).expect("valid record");
        assert_eq!(rec.fan_count, 4);
    }

    #[test]
    fn getdev_short_buffer_and_zero_denominator() {
        // buffer shorter than claimed len must not panic (hardening regression)
        assert!(parse_getdev_response(&[0x10, 0], 4).is_none());
        // off_time = 0, on_time = 0 → mobo pwm unavailable, no div-by-zero
        let resp = [0x10, 0, 0, 0];
        let report = parse_getdev_response(&resp, 4).expect("valid response");
        assert_eq!(report.mobo_pwm, None);
    }
}
