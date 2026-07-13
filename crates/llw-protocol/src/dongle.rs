//! Dongle I/O: composes the pure builders (`frames`, `record`) with the USB
//! transport. One-shot operations only — polling cadence, keepalive, and
//! recovery policy belong to the caller (daemon in M2, CLI for now).

use crate::consts::*;
use crate::frames::{self, RfFrame};
use crate::record::{parse_getdev_response, GetDevReport};
use crate::transport::{UsbTransport, USB_TIMEOUT};
use crate::{ProtocolError, Result};
use std::thread;
use std::time::Duration;
use tracing::{debug, info};

/// Master dongle identity discovered via GET_MAC.
#[derive(Debug, Clone, Copy)]
pub struct MasterInfo {
    pub mac: [u8; 6],
    pub channel: u8,
    pub firmware: Option<u16>,
}

/// An open TX/RX dongle pair. RX is optional (telemetry only) but required
/// for GetDev discovery.
pub struct Dongle {
    tx: UsbTransport,
    rx: Option<UsbTransport>,
}

/// Upstream's channel scan order: 8 first, then even 2-38, then odd 1-39.
/// (M2 replaces first-hit acquisition with scored acquisition; the order
/// helper stays useful for enumerating the space.)
pub fn default_channel_order() -> impl Iterator<Item = u8> {
    std::iter::once(8u8)
        .chain((2..=38).filter(|&ch| ch != 8 && ch % 2 == 0))
        .chain((1..=39).filter(|&ch| ch % 2 == 1))
}

impl Dongle {
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
        self.tx.write(&cmd, USB_TIMEOUT)?;

        let mut response = [0u8; 64];
        let len = match self.tx.read(&mut response, Duration::from_millis(500)) {
            Ok(len) => len,
            Err(_) => return Ok(None), // timeout = no answer on this channel
        };

        if len >= 7 && response[0] == USB_CMD_GET_MAC {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&response[1..7]);
            if mac.iter().any(|&b| b != 0) {
                let firmware = if len >= 13 {
                    Some(u16::from_be_bytes([response[11], response[12]]))
                } else {
                    None
                };
                return Ok(Some(MasterInfo { mac, channel, firmware }));
            }
        }
        Ok(None)
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
        Err(ProtocolError::Other(
            "no master answered on any channel (1-39)".into(),
        ))
    }

    /// Poll the RX for the device list (one GetDev round-trip).
    pub fn get_dev(&mut self) -> Result<GetDevReport> {
        let rx = self
            .rx
            .as_mut()
            .ok_or_else(|| ProtocolError::Other("RX dongle not available".into()))?;

        rx.read_flush();
        rx.write(&CMD_GET_DEV, USB_TIMEOUT)?;

        let mut response = [0u8; 512];
        let len = match rx.read(&mut response, Duration::from_millis(200)) {
            Ok(len) => len,
            Err(_) => {
                return Err(ProtocolError::Other(
                    "GetDev: no response (timeout)".into(),
                ))
            }
        };

        parse_getdev_response(&response, len)
            .ok_or_else(|| ProtocolError::Other(format!(
                "GetDev: unexpected response 0x{:02x}",
                response[0]
            )))
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

    /// Upload an RGB animation (or single frame) to a device. Compresses,
    /// frames, and sends header (repeated) + data packets.
    /// Returns the effect index sent (compare against future device records
    /// to detect firmware drift).
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
        if led_frames.is_empty() {
            return Err(ProtocolError::Other("no frames to upload".into()));
        }
        let led_num = led_frames[0].len() as u8;
        let total_frames = led_frames.len() as u16;

        let mut raw = Vec::with_capacity(led_frames.len() * led_num as usize * 3);
        for frame in led_frames {
            for px in frame {
                raw.extend_from_slice(px);
            }
        }
        let effect_index = frames::effect_index_from_leds(&led_frames[0]);
        let compressed = crate::tinyuz::compress(&raw)?;

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
}
