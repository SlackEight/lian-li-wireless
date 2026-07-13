//! Pure protocol library for Lian Li's 2.4GHz wireless ecosystem.
//!
//! Ported in part from `sgtaziz/lian-li-linux` (MIT) — see NOTICE.
//! This crate contains NO policy: no polling loops, no keepalive timers,
//! no recovery strategy. Callers (the daemon, the CLI) own all of that.

pub mod consts;
pub mod device_kind;
pub mod frames;
pub mod record;
pub mod tinyuz;
pub mod transport;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),

    #[error("device {vid:04x}:{pid:04x} not found")]
    DeviceNotFound { vid: u16, pid: u16 },

    #[error("compression failed: {0}")]
    Compression(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
