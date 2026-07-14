//! USB and RF protocol constants for the Lian Li wireless dongles.
//! Byte values reverse-engineered upstream from L-Connect 3's lianli.slv3.dll.

/// TX dongle VID:PID pairs (V1 Winbond, V2 CH340).
pub const TX_IDS: [(u16, u16); 2] = [(0x0416, 0x8040), (0x1A86, 0xE304)];
/// RX dongle VID:PID pairs (V1 Winbond, V2 CH340).
pub const RX_IDS: [(u16, u16); 2] = [(0x0416, 0x8041), (0x1A86, 0xE305)];

/// USB-level command bytes (first byte of each 64-byte USB packet).
pub const USB_CMD_SEND_RF: u8 = 0x10;
pub const USB_CMD_GET_MAC: u8 = 0x11;

/// RF-frame command bytes (first two bytes of the 240-byte RF frame).
pub const RF_SELECT: u8 = 0x12;
pub const RF_PWM_CMD: u8 = 0x10;
pub const RF_MASTER_CLOCK: u8 = 0x14;
pub const RF_SET_RGB: u8 = 0x20;
/// Persists the current bind table to device flash (upstream save_rf_config).
pub const RF_SAVE_CONFIG: u8 = 0x15;

/// RF frame geometry: 240-byte frames sent as 4× 60-byte chunks
/// inside 64-byte USB packets.
pub const RF_DATA_SIZE: usize = 240;
pub const RF_CHUNK_SIZE: usize = 60;
pub const RF_CHUNKS: usize = RF_DATA_SIZE / RF_CHUNK_SIZE;

/// Max compressed-RGB payload bytes per RF data packet
/// (240-byte frame minus the 20-byte RGB packet header).
pub const RGB_CHUNK_LEN: usize = 220;

const fn cmd64(b0: u8, b1: u8, b2: u8, b3: u8) -> [u8; 64] {
    let mut c = [0u8; 64];
    c[0] = b0;
    c[1] = b1;
    c[2] = b2;
    c[3] = b3;
    c
}

/// TX reset — re-syncs the RF network. Upstream sends this before polling.
pub const CMD_RESET: [u8; 64] = cmd64(0x11, 0x08, 0x00, 0x00);
/// GetDev poll (sent to RX): USB_CMD_SEND_RF + page 0x01.
pub const CMD_GET_DEV: [u8; 64] = cmd64(0x10, 0x01, 0x00, 0x00);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_command_layout() {
        assert_eq!(CMD_RESET[0], 0x11);
        assert_eq!(CMD_RESET[1], 0x08);
        assert_eq!(&CMD_RESET[2..], &[0u8; 62][..]);
        assert_eq!(CMD_RESET.len(), 64);
    }

    #[test]
    fn getdev_command_layout() {
        assert_eq!(CMD_GET_DEV[0], USB_CMD_SEND_RF);
        assert_eq!(CMD_GET_DEV[1], 0x01);
    }

    #[test]
    fn rf_geometry() {
        assert_eq!(RF_CHUNKS, 4);
        assert_eq!(RF_CHUNKS * RF_CHUNK_SIZE, RF_DATA_SIZE);
    }
}
