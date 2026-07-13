//! Link acquisition, redesigned from the M2a channel experiment:
//! the devices' GetDev records ARE the ground truth for the operating
//! channel; GET_MAC is only consulted for the master MAC (it answers on
//! any channel byte). No scanning, no channel picking.

use anyhow::{bail, Result};
use llw_protocol::dongle::Dongle;
use llw_protocol::io::UsbIo;
use llw_protocol::record::DeviceRecord;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Link {
    pub master_mac: [u8; 6],
    pub channel: u8,
}

/// Acquire the link: poll GetDev until devices appear (bounded retries),
/// adopt the channel they report, learn the master MAC.
/// `attempts` polls with ~300ms between them is the caller's cadence choice —
/// this function does NOT sleep; the caller drives retry timing (pure-ish,
/// simulation-friendly).
pub fn try_acquire<T: UsbIo>(dongle: &mut Dongle<T>) -> Result<Option<(Link, Vec<DeviceRecord>)>> {
    let report = match dongle.get_dev() {
        Ok(r) => r,
        Err(llw_protocol::ProtocolError::NoResponse { .. }) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if report.devices.is_empty() {
        return Ok(None);
    }

    // Adopt the channel the (first) device reports; verify consistency.
    let channel = report.devices[0].channel;
    if report.devices.iter().any(|d| d.channel != channel) {
        // Mixed channels = network mid-transition; treat as not-yet-acquired.
        return Ok(None);
    }

    // Master MAC: prefer the device records' master_mac (ground truth for
    // the network we're bound to); fall back to GET_MAC on the adopted channel.
    let master_mac = report.devices[0].master_mac;
    let master_mac = if master_mac.iter().any(|&b| b != 0) {
        master_mac
    } else {
        match dongle.get_mac(channel)? {
            Some(info) => info.mac,
            None => bail!("devices visible but master MAC unknown"),
        }
    };

    Ok(Some((Link { master_mac, channel }, report.devices)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use llw_protocol::io::FakeIo;

    fn record_bytes(mac: [u8; 6], master: [u8; 6], ch: u8) -> [u8; 42] {
        let mut r = [0u8; 42];
        r[0..6].copy_from_slice(&mac);
        r[6..12].copy_from_slice(&master);
        r[12] = ch;
        r[13] = 1; // rx_type
        r[19] = 3; // fans
        r[24] = 36; // SL-INF
        r[41] = 0x1C;
        r
    }

    fn getdev_resp(records: &[[u8; 42]]) -> Vec<u8> {
        let mut resp = vec![0u8; 4 + 42 * records.len()];
        resp[0] = 0x10;
        resp[1] = records.len() as u8;
        resp[2] = 0x80;
        for (i, r) in records.iter().enumerate() {
            resp[4 + i * 42..4 + (i + 1) * 42].copy_from_slice(r);
        }
        resp
    }

    const MAC: [u8; 6] = [0x02, 0x8b, 0x51, 0x62, 0x32, 0xe1];
    const MASTER: [u8; 6] = [0xe5, 0xba, 0xf0, 0x72, 0xab, 0x3c];

    #[test]
    fn adopts_device_reported_channel_and_master() {
        let rx = FakeIo::default();
        rx.push_read(getdev_resp(&[record_bytes(MAC, MASTER, 2)]));
        let mut d = Dongle::from_parts(FakeIo::default(), Some(rx));
        let (link, devs) = try_acquire(&mut d).unwrap().expect("acquired");
        assert_eq!(link, Link { master_mac: MASTER, channel: 2 });
        assert_eq!(devs.len(), 1);
    }

    #[test]
    fn empty_air_is_not_acquired() {
        let rx = FakeIo::default();
        rx.push_read({
            let mut resp = vec![0u8; 4];
            resp[0] = 0x10;
            resp[2] = 0x80;
            resp
        });
        let mut d = Dongle::from_parts(FakeIo::default(), Some(rx));
        assert!(try_acquire(&mut d).unwrap().is_none());
    }

    #[test]
    fn timeout_is_not_acquired_not_error() {
        let mut d = Dongle::from_parts(FakeIo::default(), Some(FakeIo::default()));
        assert!(try_acquire(&mut d).unwrap().is_none());
    }

    #[test]
    fn mixed_channels_treated_as_transition() {
        let rx = FakeIo::default();
        rx.push_read(getdev_resp(&[
            record_bytes(MAC, MASTER, 2),
            record_bytes([0xAA; 6], MASTER, 7),
        ]));
        let mut d = Dongle::from_parts(FakeIo::default(), Some(rx));
        assert!(try_acquire(&mut d).unwrap().is_none());
    }
}
