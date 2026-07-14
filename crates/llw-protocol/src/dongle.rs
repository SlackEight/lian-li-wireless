//! Dongle I/O: composes the pure builders (`frames`, `record`) with the USB
//! transport. One-shot operations only — polling cadence, keepalive, and
//! recovery policy belong to the caller (daemon in M2, CLI for now).

use crate::consts::*;
use crate::frames::{self, RfFrame};
use crate::io::UsbIo;
use crate::record::{parse_getdev_response, GetDevReport};
use crate::transport::{UsbTransport, USB_TIMEOUT};
use crate::{ProtocolError, Result};
use std::thread;
use std::time::Duration;
use tracing::{debug, info};

/// Master dongle identity discovered via GET_MAC.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MasterInfo {
    pub mac: [u8; 6],
    pub channel: u8,
    pub firmware: Option<u16>,
}

/// Parse a GET_MAC response. Returns None if the response is not a valid
/// echo or the MAC is all-zero (channel silent).
pub fn parse_get_mac_response(response: &[u8], len: usize, channel: u8) -> Option<MasterInfo> {
    let response = response.get(..len)?;
    if response.len() < 7 || response[0] != USB_CMD_GET_MAC {
        return None;
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&response[1..7]);
    if mac.iter().all(|&b| b == 0) {
        return None;
    }
    let firmware = if response.len() >= 13 {
        Some(u16::from_be_bytes([response[11], response[12]]))
    } else {
        None
    };
    Some(MasterInfo { mac, channel, firmware })
}

/// An open TX/RX dongle pair. RX is optional (telemetry only) but required
/// for GetDev discovery.
pub struct Dongle<T: UsbIo = UsbTransport> {
    tx: T,
    rx: Option<T>,
}

/// Upstream's channel scan order: 8 first, then even 2-38, then odd 1-39.
/// (M2 replaces first-hit acquisition with scored acquisition; the order
/// helper stays useful for enumerating the space.)
pub fn default_channel_order() -> impl Iterator<Item = u8> {
    std::iter::once(8u8)
        .chain((2..=38).filter(|&ch| ch != 8 && ch % 2 == 0))
        .chain((1..=39).filter(|&ch| ch % 2 == 1))
}

impl Dongle<UsbTransport> {
    /// Open the TX dongle (required) and RX dongle (optional), trying V1
    /// then V2 USB IDs.
    pub fn open() -> Result<Self> {
        let mut tx = open_any(&TX_IDS)?;
        tx.detach_and_configure("TX")?;

        let rx = match open_any(&RX_IDS) {
            Ok(mut rx) => {
                rx.detach_and_configure("RX")?;
                rx.read_flush();
                Some(rx)
            }
            Err(e) => {
                info!("RX dongle not found ({e}) — discovery/telemetry disabled");
                None
            }
        };

        Ok(Self { tx, rx })
    }
}

impl<T: UsbIo> Dongle<T> {
    /// Assemble a Dongle from raw parts (tests/simulations).
    pub fn from_parts(tx: T, rx: Option<T>) -> Self {
        Self { tx, rx }
    }

    pub fn has_rx(&self) -> bool {
        self.rx.is_some()
    }

    /// Send CMD_RESET to the TX dongle (re-syncs the RF network; the master
    /// may hop channels afterwards — re-run discovery).
    pub fn reset(&mut self) -> Result<()> {
        self.tx.write(&CMD_RESET, USB_TIMEOUT)?;
        thread::sleep(Duration::from_millis(500));
        Ok(())
    }

    /// Query the master MAC on one channel. Returns None if the channel
    /// doesn't answer (timeout or zero MAC).
    pub fn get_mac(&mut self, channel: u8) -> Result<Option<MasterInfo>> {
        let mut cmd = [0u8; 64];
        cmd[0] = USB_CMD_GET_MAC;
        cmd[1] = channel;
        self.tx.read_flush(); // drop stale late answers from prior scans/resets
        self.tx.write(&cmd, USB_TIMEOUT)?;

        let mut response = [0u8; 64];
        let len = match self.tx.read(&mut response, Duration::from_millis(500)) {
            Ok(len) => len,
            Err(ProtocolError::Usb(rusb::Error::Timeout)) => return Ok(None), // silent channel
            Err(e) => return Err(e),
        };

        Ok(parse_get_mac_response(&response, len, channel))
    }

    /// Survey every channel 1-39 and return all that answer.
    /// (Diagnostic; also the raw input for M2's scored acquisition.)
    pub fn survey_channels(&mut self) -> Result<Vec<MasterInfo>> {
        let mut hits = Vec::new();
        for ch in 1..=39u8 {
            if let Some(info) = self.get_mac(ch)? {
                debug!("channel {ch}: master answers");
                hits.push(info);
            }
        }
        Ok(hits)
    }

    /// Discover the master with upstream's first-hit semantics.
    pub fn discover_master(&mut self) -> Result<MasterInfo> {
        for ch in default_channel_order() {
            if let Some(info) = self.get_mac(ch)? {
                return Ok(info);
            }
        }
        Err(ProtocolError::NoMasterFound)
    }

    /// Poll the RX for the device list (one GetDev round-trip).
    pub fn get_dev(&mut self) -> Result<GetDevReport> {
        let rx = self.rx.as_mut().ok_or(ProtocolError::RxUnavailable)?;

        rx.read_flush();
        rx.write(&CMD_GET_DEV, USB_TIMEOUT)?;

        let mut response = [0u8; 512];
        let len = match rx.read(&mut response, Duration::from_millis(200)) {
            Ok(len) => len,
            Err(ProtocolError::Usb(rusb::Error::Timeout)) => {
                return Err(ProtocolError::NoResponse { op: "GetDev" })
            }
            Err(e) => return Err(e),
        };

        parse_getdev_response(&response, len).ok_or(ProtocolError::UnexpectedResponse {
            op: "GetDev",
            expected: USB_CMD_SEND_RF,
            got: response[0],
        })
    }

    /// Send one 240-byte RF frame as 4 USB chunks with the 1ms inter-chunk
    /// gap the firmware needs.
    pub fn send_rf_frame(&mut self, rf: &RfFrame, channel: u8, rx_type: u8) -> Result<()> {
        for packet in frames::usb_chunks(rf, channel, rx_type) {
            self.tx.write(&packet, USB_TIMEOUT)?;
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    /// Send a bind (or unbind) frame 6× with 30ms gaps between sends.
    ///
    /// Blocking: ~180ms+ (5 gaps × 30ms + 6 × 4-chunk USB writes).
    /// Upstream burst semantics: the repeated transmission compensates for
    /// RF packet loss during the handover window. The 30ms gap gives the
    /// device firmware time to process each attempt before the next arrives.
    /// The gap is NOT added after the final send (burst ends immediately after
    /// the 6th frame's last chunk).
    ///
    /// `frame` must be a `bind_frame(...)` result. `channel` and `rx_type`
    /// are the device's CURRENT reported channel and rx_type (not the target).
    pub fn send_bind_burst(&mut self, frame: &RfFrame, channel: u8, rx_type: u8) -> Result<()> {
        for i in 0..6u8 {
            self.send_rf_frame(frame, channel, rx_type)?;
            if i < 5 {
                thread::sleep(Duration::from_millis(30));
            }
        }
        Ok(())
    }

    /// Persist the current bind table to device flash by sending a SaveConfig
    /// frame 3× with 200ms gaps between sends.
    ///
    /// The entire 4-chunk USB write is repeated 3 times (each repetition is
    /// one `send_rf_frame` call addressed with `rx_type=0xFF`). The 200ms
    /// inter-set gap is required for flash write completion (M3 discovery:
    /// devices need an RF settle window after SaveConfig; callers should
    /// follow this call with an RF settle window of at least 200ms before
    /// resuming normal RF traffic to avoid corrupting the flash write).
    pub fn send_save_config(&mut self, master_mac: &[u8; 6], master_channel: u8) -> Result<()> {
        let frame = frames::save_config_frame(master_mac);
        for i in 0..3u8 {
            self.send_rf_frame(&frame, master_channel, 0xFF)?;
            if i < 2 {
                thread::sleep(Duration::from_millis(200));
            }
        }
        Ok(())
    }

    /// Upload an RGB animation (or single frame) to a device. Compresses,
    /// frames, and sends header (repeated) + data packets.
    /// Returns the effect index sent (compare against future device records
    /// to detect firmware drift).
    ///
    /// Blocking: a max-size animation (255 RF frames × 4 chunks with 1ms gaps,
    /// plus header repeats) holds the dongle for roughly 2-2.5 seconds; callers
    /// with latency budgets (e.g. a 1Hz heartbeat) must schedule around uploads.
    #[allow(clippy::too_many_arguments)]
    pub fn upload_rgb(
        &mut self,
        device_mac: &[u8; 6],
        master_mac: &[u8; 6],
        channel: u8,
        rx_type: u8,
        led_frames: &[Vec<[u8; 3]>],
        interval_ms: u16,
        header_repeats: u8,
    ) -> Result<[u8; 4]> {
        let (led_num, total_frames) = validate_led_frames(led_frames)?;

        let mut raw = Vec::with_capacity(led_frames.len() * led_num as usize * 3);
        for frame in led_frames {
            for px in frame {
                raw.extend_from_slice(px);
            }
        }
        let effect_index = frames::effect_index_from_frames(led_frames);
        let compressed = crate::tinyuz::compress(&raw)?;
        if compressed.len() > frames::RGB_MAX_COMPRESSED {
            return Err(ProtocolError::InvalidInput(format!(
                "compressed RGB payload {} B exceeds protocol max {} B",
                compressed.len(),
                frames::RGB_MAX_COMPRESSED
            )));
        }

        let rf_frames = frames::rgb_frames(
            device_mac,
            master_mac,
            &effect_index,
            &compressed,
            led_num,
            total_frames,
            interval_ms,
        );

        let repeats = header_repeats.max(1);
        let gap_ms = if repeats <= 2 { 2 } else { 20 };
        for (i, rf) in rf_frames.iter().enumerate() {
            if i == 0 {
                for r in 0..repeats {
                    self.send_rf_frame(rf, channel, rx_type)?;
                    if r < repeats - 1 {
                        thread::sleep(Duration::from_millis(gap_ms));
                    }
                }
            } else {
                self.send_rf_frame(rf, channel, rx_type)?;
            }
        }

        debug!(
            "uploaded RGB: {total_frames} frame(s), {led_num} LEDs, {} compressed bytes, {} RF frames",
            compressed.len(),
            rf_frames.len()
        );
        Ok(effect_index)
    }
}

/// Validate an animation's shape. Returns (led_num, total_frames).
fn validate_led_frames(led_frames: &[Vec<[u8; 3]>]) -> Result<(u8, u16)> {
    let first = led_frames
        .first()
        .ok_or_else(|| ProtocolError::InvalidInput("no frames to upload".into()))?;
    if first.is_empty() || first.len() > 255 {
        return Err(ProtocolError::InvalidInput(format!(
            "LED count {} out of range 1-255",
            first.len()
        )));
    }
    if led_frames.len() > u16::MAX as usize {
        return Err(ProtocolError::InvalidInput(format!(
            "{} frames exceeds u16 range",
            led_frames.len()
        )));
    }
    if let Some(bad) = led_frames.iter().find(|f| f.len() != first.len()) {
        return Err(ProtocolError::InvalidInput(format!(
            "ragged animation: frame with {} LEDs, expected {}",
            bad.len(),
            first.len()
        )));
    }
    Ok((first.len() as u8, led_frames.len() as u16))
}

fn open_any(ids: &[(u16, u16)]) -> Result<UsbTransport> {
    let mut last_err = None;
    for &(vid, pid) in ids {
        match UsbTransport::open(vid, pid) {
            Ok(t) => return Ok(t),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or(ProtocolError::Other("no VID:PID pairs to try".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::FakeIo;

    fn getdev_response_with_one_device() -> Vec<u8> {
        let rec = crate::record::tests::make_record(
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            2, 3, 0, 2, [36, 36, 0, 0], [700, 700, 0, 0], [86, 86, 0, 0],
        );
        let mut resp = vec![0u8; 4 + 42];
        resp[0] = 0x10;
        resp[1] = 1;
        resp[2] = 0x80; // mobo pwm unavailable
        resp[4..46].copy_from_slice(&rec);
        resp
    }

    #[test]
    fn get_dev_via_fake_io() {
        let rx = FakeIo::default();
        rx.push_read(getdev_response_with_one_device());
        let mut d = Dongle::from_parts(FakeIo::default(), Some(rx));
        let report = d.get_dev().expect("parsed");
        assert_eq!(report.devices.len(), 1);
        assert_eq!(report.devices[0].fan_count, 2);
    }

    #[test]
    fn get_dev_timeout_is_typed() {
        let mut d = Dongle::from_parts(FakeIo::default(), Some(FakeIo::default()));
        assert!(matches!(
            d.get_dev(),
            Err(crate::ProtocolError::NoResponse { op: "GetDev" })
        ));
    }

    #[test]
    fn get_mac_timeout_is_silent_channel() {
        let mut d = Dongle::from_parts(FakeIo::default(), None);
        assert_eq!(d.get_mac(5).expect("ok"), None);
        // and the write actually went out: [0x11, channel, 0...]
        let writes = d.tx.written();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0][0], 0x11);
        assert_eq!(writes[0][1], 5);
    }

    #[test]
    fn send_rf_frame_chunks_via_fake_io() {
        let mut d = Dongle::from_parts(FakeIo::default(), None);
        let rf = crate::frames::pwm_frame(
            &[0xAA; 6], &[0x11; 6], 3, 2, 1, &[100, 100, 0, 0],
        );
        d.send_rf_frame(&rf, 2, 3).expect("sent");
        let writes = d.tx.written();
        assert_eq!(writes.len(), 4); // 4 USB chunks
        for (i, w) in writes.iter().enumerate() {
            assert_eq!(w[0], 0x10);
            assert_eq!(w[1], i as u8);
            assert_eq!(w[2], 2);
            assert_eq!(w[3], 3);
        }
    }

    #[test]
    fn channel_order_matches_upstream() {
        let order: Vec<u8> = default_channel_order().collect();
        assert_eq!(order[0], 8);
        assert_eq!(order.len(), 39); // every channel 1-39 exactly once
        let mut sorted = order.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, (1..=39).collect::<Vec<u8>>());
        // even channels (except 8) before odd
        let idx_of = |ch: u8| order.iter().position(|&c| c == ch).unwrap();
        assert!(idx_of(2) < idx_of(1));
        assert!(idx_of(38) < idx_of(39));
    }

    #[test]
    fn parses_get_mac_response() {
        let mut resp = [0u8; 64];
        resp[0] = 0x11;
        resp[1..7].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        resp[11] = 0x01;
        resp[12] = 0x2C;
        let info = parse_get_mac_response(&resp, 13, 5).expect("valid");
        assert_eq!(info.mac, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(info.channel, 5);
        assert_eq!(info.firmware, Some(300));

        let info = parse_get_mac_response(&resp, 7, 5).expect("valid");
        assert_eq!(info.firmware, None);

        let mut bad = resp;
        bad[0] = 0x10;
        assert!(parse_get_mac_response(&bad, 13, 5).is_none());

        let mut zero = [0u8; 64];
        zero[0] = 0x11;
        assert!(parse_get_mac_response(&zero, 13, 5).is_none());

        assert!(parse_get_mac_response(&resp[..2], 13, 5).is_none()); // len > buffer: no panic
    }

    #[test]
    fn send_bind_burst_writes_24_chunks_with_correct_headers() {
        let mut d = Dongle::from_parts(FakeIo::default(), None);
        let rf = crate::frames::bind_frame(
            &[0xAA; 6], &[0x11; 6], 7, 3, &[100, 100, 0, 0],
        );
        let channel: u8 = 3;
        let rx_type: u8 = 7;
        d.send_bind_burst(&rf, channel, rx_type).expect("sent");
        let writes = d.tx.written();
        // 6 frames × 4 chunks = 24 USB writes
        assert_eq!(writes.len(), 24, "burst must be exactly 6×4=24 writes");
        for (i, w) in writes.iter().enumerate() {
            // Every packet carries the correct channel and rx_type
            assert_eq!(w[2], channel, "packet[{i}][2] must be channel");
            assert_eq!(w[3], rx_type, "packet[{i}][3] must be rx_type");
        }
    }

    #[test]
    fn send_save_config_writes_12_chunks_with_0xff_rx() {
        let mut d = Dongle::from_parts(FakeIo::default(), None);
        let master_mac: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        let master_channel: u8 = 5;
        d.send_save_config(&master_mac, master_channel).expect("sent");
        let writes = d.tx.written();
        // 3 sends × 4 chunks = 12 USB writes
        assert_eq!(writes.len(), 12, "save_config must be exactly 3×4=12 writes");
        for (i, w) in writes.iter().enumerate() {
            assert_eq!(w[2], master_channel, "packet[{i}][2] must be master_channel");
            assert_eq!(w[3], 0xFF, "packet[{i}][3] must be 0xFF (rx_type for SaveConfig)");
        }
    }

    #[test]
    fn validates_led_frames() {
        assert!(validate_led_frames(&[]).is_err());
        assert!(validate_led_frames(&[vec![]]).is_err());
        assert!(validate_led_frames(&[vec![[0, 0, 0]; 256]]).is_err());
        assert!(validate_led_frames(&[vec![[0, 0, 0]; 4], vec![[0, 0, 0]; 5]]).is_err());
        let (n, t) = validate_led_frames(&[
            vec![[0, 0, 0]; 44],
            vec![[0, 0, 0]; 44],
            vec![[0, 0, 0]; 44],
        ])
        .unwrap();
        assert_eq!((n, t), (44, 3));
    }
}
