# M1: Protocol Port + CLI Proof — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working `llw-protocol` crate (ported from upstream `sgtaziz/lian-li-linux` @ `d262007`, MIT) plus an `llw` CLI that proves it on real hardware: fans spin at commanded PWM, and a static color lights the SL-INF fans and the Strimer Wireless.

**Architecture:** Pure-logic modules (frame builders, record parsing, device classification) are separated from I/O (USB transport, dongle wrapper) so everything except the actual USB writes is unit-tested without hardware. No policy in the library — no polling loops, no keepalive timers, no recovery tiers (those are M2, in `llw-daemon`). The CLI composes one-shot operations.

**Tech Stack:** Rust (edition 2021, workspace), `rusb` 0.9, `thiserror` 2, `tracing` 0.1, `clap` 4.5 (CLI only), `anyhow` (CLI only), `cc` (build-dep, compiles vendored tinyuz C++), git submodules `sisong/tinyuz` + `sisong/HDiffPatch`.

**Context for the engineer:**
- Spec: `docs/superpowers/specs/2026-07-13-lian-li-wireless-design.md` (read §3.1, §5 before starting).
- Upstream source to port from is referenced as `.upstream/` (a git worktree created in Task 1). All upstream paths below are relative to it.
- Everything runs on the owner's machine (CachyOS). The dongles are USB `0416:8040` (TX) / `0416:8041` (RX). udev rules from the installed `lianli-linux-git` package already grant mode 0666 on these, so no root needed.
- **CRITICAL for hardware tasks:** the existing daemon owns the dongles. Before any hardware test: `systemctl --user stop lianli-watchdog.service lianli-daemon.service` (watchdog first — it restarts the daemon). Re-enable after: `systemctl --user start lianli-daemon.service lianli-watchdog.service`.
- Work directly on `main` — this is a greenfield repo, no worktree isolation needed.

---

## File structure (end state of M1)

```
lian-li-wireless/
├── Cargo.toml                        # workspace: crates/*, resolver 2
├── .gitignore                        # /target, /.upstream
├── .gitmodules                       # vendor/tinyuz, vendor/HDiffPatch
├── NOTICE                            # attribution to sgtaziz/lian-li-linux
├── README.md
├── vendor/
│   ├── tuz_wrapper.cpp               # copied from upstream (blob c9e0ddc)
│   ├── tinyuz/                       # submodule @ d66d58edd2d67c23c899ff3017466472dcd50c3b
│   └── HDiffPatch/                   # submodule @ e8095214e3e20cb1562e88e4627da3ecb75710bd
└── crates/
    ├── llw-protocol/
    │   ├── Cargo.toml
    │   ├── build.rs                  # cc build of vendored tinyuz
    │   └── src/
    │       ├── lib.rs                # pub mod tree + ProtocolError
    │       ├── consts.rs             # USB/RF commands, dongle IDs, sizes
    │       ├── device_kind.rs        # DeviceKind (upstream WirelessFanType)
    │       ├── record.rs             # DeviceRecord + parse (42-byte records, GetDev response)
    │       ├── frames.rs             # pure RF frame builders + PWM constraints + FNV effect index
    │       ├── tinyuz.rs             # FFI wrapper (compress)
    │       ├── transport.rs          # UsbTransport (rusb-only port of upstream usb.rs)
    │       └── dongle.rs             # Dongle: open TX/RX, get_mac scan, reset, get_dev, send RF, upload RGB
    └── llw-cli/
        ├── Cargo.toml
        └── src/main.rs               # llw scan|devices|reset|set-pwm|set-color
```

Responsibilities: `consts`/`device_kind`/`record`/`frames` are pure (no I/O, fully unit-tested). `transport` is the thin rusb wrapper. `dongle` is the only module that composes I/O + pure logic. `llw-cli` is a throwaway-quality proof tool (it becomes a debug utility later; the daemon is M2).

---

### Task 1: Workspace scaffolding + upstream reference checkout

**Files:**
- Create: `Cargo.toml`
- Create: `.gitignore`

- [ ] **Step 1: Create the upstream reference worktree**

```bash
git -C /home/morganblem/lian-li-linux-pr worktree add /home/morganblem/lian-li-wireless/.upstream d262007
```

Expected: `HEAD is now at d262007 daemon: add SetRgbFrames IPC to upload onboard wireless animations (#93)`

- [ ] **Step 2: Create root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
anyhow = "1.0"
thiserror = "2.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
rusb = "0.9"
clap = { version = "4.5", features = ["derive"] }
```

- [ ] **Step 3: Create `.gitignore`**

```gitignore
/target
/.upstream
```

- [ ] **Step 4: Verify and commit**

```bash
cd /home/morganblem/lian-li-wireless && cargo check 2>&1 | tail -2
```

Expected: error `contains no package: The manifest is virtual, and the workspace has no members` is **fine at this stage** — actually with `members = ["crates/*"]` and no crates dir, cargo errors with `failed to load manifest ... crates/* ... doesn't match any packages`. Either message is acceptable; what matters is the TOML parses (no `invalid TOML` error).

```bash
git add Cargo.toml .gitignore && git commit -m "chore: workspace scaffolding"
```

---

### Task 2: llw-protocol skeleton + vendored tinyuz compression

Upstream compresses RGB payloads with tinyuz (LZ77 for embedded; the fan firmware decompresses it). Upstream vendors it as two git submodules plus a C++ FFI wrapper, built by `cc` in `build.rs`. We replicate that exactly, pinned to the same commits.

**Files:**
- Create: `vendor/tuz_wrapper.cpp` (copy from upstream)
- Create: `.gitmodules` (via `git submodule add`)
- Create: `crates/llw-protocol/Cargo.toml`
- Create: `crates/llw-protocol/build.rs`
- Create: `crates/llw-protocol/src/lib.rs`
- Create: `crates/llw-protocol/src/tinyuz.rs`

- [ ] **Step 1: Add the pinned submodules and wrapper**

```bash
cd /home/morganblem/lian-li-wireless
mkdir -p vendor crates/llw-protocol/src
git submodule add https://github.com/sisong/tinyuz.git vendor/tinyuz
git -C vendor/tinyuz checkout d66d58edd2d67c23c899ff3017466472dcd50c3b
git submodule add https://github.com/sisong/HDiffPatch.git vendor/HDiffPatch
git -C vendor/HDiffPatch checkout e8095214e3e20cb1562e88e4627da3ecb75710bd
cp .upstream/vendor/tuz_wrapper.cpp vendor/tuz_wrapper.cpp
git add .gitmodules vendor
```

Note: the scratch worktree `.upstream/` does not have submodules initialized — that's fine, we clone them fresh from GitHub at the pinned SHAs above (verified identical to upstream's gitlinks at `d262007`).

- [ ] **Step 2: Create `crates/llw-protocol/Cargo.toml`**

```toml
[package]
name = "llw-protocol"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Pure protocol library for Lian Li 2.4GHz wireless dongles (TX/RX) and devices"

[dependencies]
rusb = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

[build-dependencies]
cc = "1.0"
```

- [ ] **Step 3: Create `crates/llw-protocol/build.rs`**

Port of `.upstream/crates/lianli-devices/build.rs`, unchanged except the doc comment:

```rust
/// Compiles the vendored tinyuz C++ library (RGB payload compression —
/// the wireless device firmware decompresses this format).
fn main() {
    let vendor = std::path::Path::new("../../vendor");
    let tinyuz = vendor.join("tinyuz");
    let hdiff = vendor.join("HDiffPatch");

    let cpp_sources = [
        vendor.join("tuz_wrapper.cpp"),
        tinyuz.join("compress/tuz_enc.cpp"),
        tinyuz.join("compress/tuz_enc_private/tuz_enc_clip.cpp"),
        tinyuz.join("compress/tuz_enc_private/tuz_enc_code.cpp"),
        tinyuz.join("compress/tuz_enc_private/tuz_enc_match.cpp"),
        tinyuz.join("compress/tuz_enc_private/tuz_sstring.cpp"),
        hdiff.join("libHDiffPatch/HDiff/private_diff/libdivsufsort/divsufsort.cpp"),
    ];

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .opt_level(3)
        .define("NDEBUG", None)
        .define("_IS_USED_MULTITHREAD", "0")
        .include(vendor)
        .include(&tinyuz)
        .include(&hdiff)
        .warnings(false);

    for src in &cpp_sources {
        build.file(src);
    }

    build.compile("tinyuz");

    println!("cargo:rerun-if-changed=../../vendor/tuz_wrapper.cpp");
    println!("cargo:rerun-if-changed=../../vendor/tinyuz/compress/");
}
```

- [ ] **Step 4: Create `crates/llw-protocol/src/lib.rs`**

```rust
//! Pure protocol library for Lian Li's 2.4GHz wireless ecosystem.
//!
//! Ported in part from `sgtaziz/lian-li-linux` (MIT) — see NOTICE.
//! This crate contains NO policy: no polling loops, no keepalive timers,
//! no recovery strategy. Callers (the daemon, the CLI) own all of that.

pub mod consts;
pub mod tinyuz;

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
```

(`consts` is written in Task 3 — create it as an empty file for now so this compiles: `touch crates/llw-protocol/src/consts.rs`.)

- [ ] **Step 5: Create `crates/llw-protocol/src/tinyuz.rs` with its tests**

Port of `.upstream/crates/lianli-devices/src/tinyuz.rs`; only the error type changes (`anyhow` → `ProtocolError`):

```rust
//! Rust FFI bindings for the vendored tinyuz compression library.
//!
//! tinyuz is an LZ77 variant designed for embedded systems.
//! The wireless device firmware uses it to decompress RGB frame data.

use crate::{ProtocolError, Result};
use std::os::raw::c_uchar;

extern "C" {
    fn tuz_compress_mem(
        input: *const c_uchar,
        input_len: usize,
        output: *mut c_uchar,
        output_capacity: usize,
        dict_size: usize,
    ) -> usize;

    fn tuz_max_compressed_size(input_len: usize) -> usize;
}

/// Default dictionary size (4KB) — must match what the device firmware expects.
const DICT_SIZE_4K: usize = 4096;

/// Compress data using tinyuz with a 4KB dictionary.
pub fn compress(input: &[u8]) -> Result<Vec<u8>> {
    if input.is_empty() {
        return Err(ProtocolError::Compression(
            "cannot compress empty input".into(),
        ));
    }

    let max_size = unsafe { tuz_max_compressed_size(input.len()) };
    let mut output = vec![0u8; max_size];

    let compressed_len = unsafe {
        tuz_compress_mem(
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
            DICT_SIZE_4K,
        )
    };

    if compressed_len == 0 {
        return Err(ProtocolError::Compression("compressor returned 0".into()));
    }

    output.truncate(compressed_len);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_solid_color() {
        let mut rgb_data = Vec::new();
        for _ in 0..20 {
            rgb_data.extend_from_slice(&[255, 0, 0]);
        }
        let compressed = compress(&rgb_data).expect("compression should succeed");
        assert!(!compressed.is_empty());
        assert!(compressed.len() < rgb_data.len());
    }

    #[test]
    fn compress_gradient() {
        let mut rgb_data = Vec::new();
        for i in 0..80u8 {
            rgb_data.extend_from_slice(&[i, i, i]);
        }
        let compressed = compress(&rgb_data).expect("compression should succeed");
        assert!(!compressed.is_empty());
    }

    #[test]
    fn compress_empty_fails() {
        assert!(compress(&[]).is_err());
    }
}
```

- [ ] **Step 6: Run the tests**

```bash
touch crates/llw-protocol/src/consts.rs
cargo test -p llw-protocol 2>&1 | tail -5
```

Expected: `test result: ok. 3 passed` (the cc build compiles the C++ — needs `gcc`; it's present on this machine).

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(protocol): crate skeleton + vendored tinyuz compression (from sgtaziz/lian-li-linux, MIT)"
```

---

### Task 3: Protocol constants

**Files:**
- Modify: `crates/llw-protocol/src/consts.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write `consts.rs` (constants + failing test together — the test asserts command layout)**

```rust
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

/// RF frame geometry: 240-byte frames sent as 4× 60-byte chunks
/// inside 64-byte USB packets.
pub const RF_DATA_SIZE: usize = 240;
pub const RF_CHUNK_SIZE: usize = 60;
pub const RF_CHUNKS: usize = RF_DATA_SIZE / RF_CHUNK_SIZE;

/// Max compressed-RGB payload bytes per RF data packet.
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
```

Upstream cross-reference: values match `.upstream/crates/lianli-devices/src/wireless/mod.rs:39-59`. We intentionally do NOT port `CMD_VIDEO_START` / `CMD_RX_QUERY_*` / `CMD_RX_LCD_MODE` (LCD-streaming machinery — post-v1) or the AIO constants (no wireless AIO in v1 hardware).

- [ ] **Step 2: Run tests**

```bash
cargo test -p llw-protocol consts 2>&1 | tail -3
```

Expected: `3 passed`.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(protocol): USB/RF command constants"
```

---

### Task 4: DeviceKind classification

**Files:**
- Create: `crates/llw-protocol/src/device_kind.rs`
- Modify: `crates/llw-protocol/src/lib.rs` (add `pub mod device_kind;`)

Port of `.upstream/crates/lianli-devices/src/wireless/fan_type.rs` (renamed `WirelessFanType` → `DeviceKind`; "fan type" is misleading for LED strips). AIO-specific helpers (`pump_rpm_range`, `pump_led_count`) are ported too — they're 6 lines and Strimer/fan classification references `is_aio`.

- [ ] **Step 1: Write the failing tests first** (at the bottom of the new file, with a stub enum so it compiles — or write tests + full port together and verify tests pass; for a direct port, write the complete file below and confirm all tests pass)

- [ ] **Step 2: Write `device_kind.rs` (complete file)**

```rust
//! Classification of wireless device kinds from the GetDev record bytes.
//! Determines LED geometry, minimum PWM duty, and display names.

/// Wireless device kind.
///
/// Classification inputs (from the 42-byte device record):
/// - `device_type` byte [18]: 10/11 = AIOs, 1-9 = Strimer, 65/66/88 = case devices
/// - otherwise the per-slot fan-type bytes [24..28]:
///   SLV3 LED 20-23, SLV3 LCD 24-26, TLV2 LCD 27|32-35, TLV2 LED 28-31,
///   SL-INF 36-39, RL120 40, CLV1 41-42
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// SLV3 LED fans (no LCD) — 14% minimum duty, 40 LEDs/fan
    Slv3Led,
    /// SLV3 LCD fans — 14% minimum duty, 40 LEDs/fan
    Slv3Lcd,
    /// TLV2 LCD fans — 10% minimum duty, 26 LEDs/fan
    Tlv2Lcd,
    /// TLV2 LED fans — 11% minimum duty, 26 LEDs/fan
    Tlv2Led,
    /// SL-INF wireless fans — 11% minimum duty, 44 LEDs/fan
    SlInf,
    /// CL / RL120 fans — 10% minimum duty, 24 LEDs/fan (special PWM filter)
    Clv1,
    /// HydroShift II LCD-C (Circle) wireless AIO (device_type 10)
    WaterBlock,
    /// HydroShift II LCD-S (Square) wireless AIO (device_type 11)
    WaterBlock2,
    /// Strimer Wireless LED strip (device_type 1-9) — RGB only, no fans
    Strimer(u8),
    /// Lancool 217 case RGB ring (device_type 65) — 96 LEDs
    Lc217,
    /// Universal Screen 8.8" LED ring (device_type 88) — 88 LEDs
    Led88,
    /// Lancool V150 controller (device_type 66) — 88 LEDs dual-zone
    V150,
    /// Unknown device kind
    Unknown,
}

impl DeviceKind {
    /// Minimum PWM duty percentage the hardware accepts for a nonzero speed.
    pub fn min_duty_percent(self) -> u8 {
        match self {
            Self::Slv3Led | Self::Slv3Lcd => 14,
            Self::Tlv2Lcd => 10,
            Self::Tlv2Led | Self::SlInf => 11,
            Self::Clv1 | Self::WaterBlock | Self::WaterBlock2 | Self::V150 => 10,
            Self::Strimer(_) | Self::Lc217 | Self::Led88 => 0,
            Self::Unknown => 10,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Slv3Led => "UNI FAN SL V3 Wireless",
            Self::Slv3Lcd => "UNI FAN SL V3 Wireless LCD",
            Self::Tlv2Lcd => "UNI FAN TL Wireless LCD",
            Self::Tlv2Led => "UNI FAN TL Wireless",
            Self::SlInf => "UNI FAN SL-INF Wireless",
            Self::Clv1 => "UNI FAN CL Wireless",
            Self::WaterBlock => "HydroShift II LCD-C (Wireless)",
            Self::WaterBlock2 => "HydroShift II LCD-S (Wireless)",
            Self::Strimer(_) => "Strimer Wireless",
            Self::Lc217 => "Lancool 217 Wireless",
            Self::Led88 => "Universal Screen 8.8\" Wireless",
            Self::V150 => "Lancool V150 Wireless",
            Self::Unknown => "Wireless Device",
        }
    }

    pub fn leds_per_fan(self) -> u8 {
        match self {
            Self::Tlv2Lcd | Self::Tlv2Led => 26,
            Self::Slv3Led | Self::Slv3Lcd => 40,
            Self::SlInf => 44,
            Self::Clv1 | Self::WaterBlock | Self::WaterBlock2 => 24,
            Self::Strimer(_) | Self::Lc217 | Self::Led88 | Self::V150 => 0,
            Self::Unknown => 20,
        }
    }

    pub fn is_aio(self) -> bool {
        matches!(self, Self::WaterBlock | Self::WaterBlock2)
    }

    pub fn is_rgb_only(self) -> bool {
        matches!(self, Self::Strimer(_) | Self::Lc217 | Self::Led88)
    }

    pub fn pump_led_count(self) -> u8 {
        if self.is_aio() {
            24
        } else {
            0
        }
    }

    /// Total LED count for flat-buffer (non per-fan) devices.
    pub fn led_count_override(self) -> Option<u16> {
        match self {
            Self::Strimer(dt) => Some(match dt {
                1 => 116,
                2 => 132,
                3 => 174,
                _ => 88,
            }),
            Self::Lc217 => Some(96),
            Self::Led88 => Some(88),
            Self::V150 => Some(88),
            _ => None,
        }
    }

    /// Classify from a per-slot fan-type byte in the device record.
    pub fn from_fan_type_byte(b: u8) -> Self {
        match b {
            20..=23 => Self::Slv3Led,
            24..=26 => Self::Slv3Lcd,
            27 | 32..=35 => Self::Tlv2Lcd,
            28..=31 => Self::Tlv2Led,
            36..=39 => Self::SlInf,
            40..=42 => Self::Clv1,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sl_inf_classification_and_geometry() {
        for b in 36..=39u8 {
            assert_eq!(DeviceKind::from_fan_type_byte(b), DeviceKind::SlInf);
        }
        assert_eq!(DeviceKind::SlInf.leds_per_fan(), 44);
        assert_eq!(DeviceKind::SlInf.min_duty_percent(), 11);
        assert!(!DeviceKind::SlInf.is_rgb_only());
    }

    #[test]
    fn strimer_led_counts_by_subtype() {
        assert_eq!(DeviceKind::Strimer(1).led_count_override(), Some(116));
        assert_eq!(DeviceKind::Strimer(2).led_count_override(), Some(132));
        assert_eq!(DeviceKind::Strimer(3).led_count_override(), Some(174));
        assert_eq!(DeviceKind::Strimer(9).led_count_override(), Some(88));
        assert!(DeviceKind::Strimer(2).is_rgb_only());
        assert_eq!(DeviceKind::Strimer(2).min_duty_percent(), 0);
    }

    #[test]
    fn boundary_bytes() {
        assert_eq!(DeviceKind::from_fan_type_byte(19), DeviceKind::Unknown);
        assert_eq!(DeviceKind::from_fan_type_byte(20), DeviceKind::Slv3Led);
        assert_eq!(DeviceKind::from_fan_type_byte(27), DeviceKind::Tlv2Lcd);
        assert_eq!(DeviceKind::from_fan_type_byte(28), DeviceKind::Tlv2Led);
        assert_eq!(DeviceKind::from_fan_type_byte(43), DeviceKind::Unknown);
    }
}
```

- [ ] **Step 3: Register the module and run tests**

Add `pub mod device_kind;` to `lib.rs`, then:

```bash
cargo test -p llw-protocol device_kind 2>&1 | tail -3
```

Expected: `3 passed`.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(protocol): DeviceKind classification (port of upstream WirelessFanType)"
```

---

### Task 5: Device record + GetDev response parsing

**Files:**
- Create: `crates/llw-protocol/src/record.rs`
- Modify: `crates/llw-protocol/src/lib.rs` (add `pub mod record;`)

Port of the parsing half of `.upstream/crates/lianli-devices/src/wireless/discovery.rs` — as **pure functions** (upstream mixes parsing into the I/O polling function; we split so it's testable). The I/O half goes into `dongle.rs` (Task 8).

- [ ] **Step 1: Write `record.rs` (complete file, tests included)**

```rust
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
    if len < 4 || response[0] != crate::consts::USB_CMD_SEND_RF {
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
    if device_count == 0 || device_count > 12 {
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
}
```

- [ ] **Step 2: Register the module and run tests**

Add `pub mod record;` to `lib.rs`, then:

```bash
cargo test -p llw-protocol record 2>&1 | tail -3
```

Expected: `6 passed`.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(protocol): 42-byte device record + GetDev response parsing (pure)"
```

---

### Task 6: Pure RF frame builders

**Files:**
- Create: `crates/llw-protocol/src/frames.rs`
- Modify: `crates/llw-protocol/src/lib.rs` (add `pub mod frames;`)

The heart of the protocol. Upstream builds these frames inline inside I/O methods (`fan_speed.rs`, `rgb.rs`, `controller.rs::send_master_clock`); we extract them as pure functions returning `[u8; 240]` frames so every byte is testable. The I/O layer (Task 8) only chunks and writes.

- [ ] **Step 1: Write `frames.rs` (complete file, tests included)**

```rust
//! Pure builders for the 240-byte RF frames and their 64-byte USB chunks.
//! No I/O here — byte-exact and fully unit-tested.

use crate::consts::*;
use crate::device_kind::DeviceKind;

pub type RfFrame = [u8; RF_DATA_SIZE];

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
/// Returns the header frame first (send it `header_repeats` times), then the
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
}
```

- [ ] **Step 2: Register the module and run tests**

Add `pub mod frames;` to `lib.rs`, then:

```bash
cargo test -p llw-protocol frames 2>&1 | tail -3
```

Expected: `7 passed`.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(protocol): pure RF frame builders (PWM, master clock, RGB upload) + constraints + FNV effect index"
```

---

### Task 7: USB transport

**Files:**
- Create: `crates/llw-protocol/src/transport.rs`
- Modify: `crates/llw-protocol/src/lib.rs` (add `pub mod transport;`)

Direct port of `.upstream/crates/lianli-transport/src/usb.rs` with these exact deletions/changes (the dongles are the only devices we talk to — no HID, no LCD):

1. Error type: use `crate::ProtocolError` instead of upstream's `TransportError` (variants `Usb` and `DeviceNotFound` already exist in ours from Task 2).
2. **Delete** consts `LCD_WRITE_TIMEOUT`, `LCD_READ_TIMEOUT`.
3. **Delete** functions: `open_device`, `control_in`, `control_out`, `clear_halt`, `inner`, `read_serial`, `find_usb_devices`.
4. **Keep** everything else verbatim: `EP_OUT`/`EP_IN`/`USB_TIMEOUT` consts, the `UsbTransport` struct with `claimed` interface tracking, `open`, `detach_and_configure` (including both USB-reset recovery paths), `write` (with short-write warning), `read`, `read_flush`, `release`, `reset`, the `Drop` impl (release + re-attach kernel driver), and `detect_endpoint_types`.

- [ ] **Step 1: Copy and edit**

```bash
cp .upstream/crates/lianli-transport/src/usb.rs crates/llw-protocol/src/transport.rs
```

Then apply the edits above. The import block becomes:

```rust
use crate::ProtocolError;
use rusb::{Device, DeviceHandle, GlobalContext};
use std::time::Duration;
use tracing::{debug, info, warn};
```

and every `Result<_, TransportError>` becomes `Result<_, ProtocolError>` (rusb errors convert via the existing `#[from]`). `open` returns `ProtocolError::DeviceNotFound { vid, pid }` when no device matches.

- [ ] **Step 2: Register module, build, run full test suite**

Add `pub mod transport;` to `lib.rs`, then:

```bash
cargo test -p llw-protocol 2>&1 | tail -3
```

Expected: all previous tests still pass (transport itself has no unit tests — it requires hardware; it gets exercised in Task 10).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(protocol): USB transport (rusb-only port of upstream usb.rs)"
```

---

### Task 8: Dongle I/O layer

**Files:**
- Create: `crates/llw-protocol/src/dongle.rs`
- Modify: `crates/llw-protocol/src/lib.rs` (add `pub mod dongle;`)

The only module composing I/O with the pure builders. Deliberately **synchronous, single-owner, no Arc/Mutex/threads** — upstream's `WirelessController` bundles polling threads and recovery policy; in our architecture those live in the M2 daemon.

- [ ] **Step 1: Write `dongle.rs` (complete file)**

```rust
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
```

- [ ] **Step 2: Register module and run all tests**

Add `pub mod dongle;` to `lib.rs`, then:

```bash
cargo test -p llw-protocol 2>&1 | tail -3
```

Expected: all tests pass (the new `channel_order_matches_upstream` plus all prior).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(protocol): Dongle I/O layer (open, scan, GetDev, RF send, RGB upload)"
```

---

### Task 9: llw CLI

**Files:**
- Create: `crates/llw-cli/Cargo.toml`
- Create: `crates/llw-cli/src/main.rs`

- [ ] **Step 1: Create `crates/llw-cli/Cargo.toml`**

```toml
[package]
name = "llw-cli"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "llw"
path = "src/main.rs"

[dependencies]
llw-protocol = { path = "../llw-protocol" }
anyhow = { workspace = true }
clap = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 2: Write `crates/llw-cli/src/main.rs` (complete file)**

```rust
//! `llw` — hardware proof CLI for llw-protocol (M1).
//! One-shot operations; run with RUST_LOG=debug for wire-level tracing.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use llw_protocol::dongle::Dongle;
use llw_protocol::frames::{apply_pwm_constraints, pwm_frame};
use llw_protocol::record::DeviceRecord;

#[derive(Parser)]
#[command(name = "llw", about = "Lian Li wireless protocol proof tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Survey all RF channels (1-39) for master responses
    Scan,
    /// List wireless devices reported by the RX dongle
    Devices,
    /// Send CMD_RESET to the TX dongle (master may hop channels)
    Reset,
    /// Set fan PWM on a device
    SetPwm {
        /// Device index from `llw devices`
        index: u8,
        /// Duty cycle percent (0-100), applied to all fan slots
        percent: u8,
        /// Re-send every second until Ctrl+C (fans revert without keepalive)
        #[arg(long)]
        hold: bool,
    },
    /// Set a static color on a device (single-frame onboard upload)
    SetColor {
        /// Device index from `llw devices`
        index: u8,
        /// Hex color, e.g. FF0000
        color: String,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Command::Scan => scan(),
        Command::Devices => devices(),
        Command::Reset => reset(),
        Command::SetPwm { index, percent, hold } => set_pwm(index, percent, hold),
        Command::SetColor { index, color } => set_color(index, &color),
    }
}

fn scan() -> Result<()> {
    let mut dongle = Dongle::open().context("opening dongles")?;
    println!("Surveying channels 1-39...");
    let hits = dongle.survey_channels()?;
    if hits.is_empty() {
        println!("No master answered on any channel.");
    }
    for h in &hits {
        println!(
            "  channel {:>2}: master {}  fw={}",
            h.channel,
            mac_str(&h.mac),
            h.firmware.map_or("?".into(), |f| f.to_string()),
        );
    }
    Ok(())
}

fn devices() -> Result<()> {
    let mut dongle = Dongle::open().context("opening dongles")?;
    if !dongle.has_rx() {
        bail!("RX dongle not found — cannot list devices");
    }
    let report = poll_devices(&mut dongle)?;
    match report.mobo_pwm {
        Some(pwm) => println!("Motherboard PWM: {pwm}/255"),
        None => println!("Motherboard PWM: unavailable"),
    }
    println!("{} device(s):", report.devices.len());
    for d in &report.devices {
        println!(
            "  [{}] {} — {} | ch={} rx={} fans={} rpm={:?} pwm={:?} fx={:02x?}",
            d.list_index,
            mac_str(&d.mac),
            d.kind.display_name(),
            d.channel,
            d.rx_type,
            d.fan_count,
            d.fan_rpms,
            d.current_pwm,
            d.effect_index,
        );
    }
    Ok(())
}

fn reset() -> Result<()> {
    let mut dongle = Dongle::open().context("opening dongles")?;
    dongle.reset()?;
    println!("CMD_RESET sent. Master may hop channels — run `llw scan` to re-locate.");
    Ok(())
}

fn set_pwm(index: u8, percent: u8, hold: bool) -> Result<()> {
    if percent > 100 {
        bail!("percent must be 0-100");
    }
    let mut dongle = Dongle::open().context("opening dongles")?;
    let master = dongle.discover_master().context("discovering master")?;
    println!("Master {} on channel {}", mac_str(&master.mac), master.channel);

    let device = find_device(&mut dongle, index)?;
    let raw = (percent as u16 * 255 / 100) as u8;
    let mut pwm = [raw; 4];
    apply_pwm_constraints(&mut pwm, device.kind, device.fan_count);
    // seq_index: position among bound devices + 1 (single-device systems: 1)
    let rf = pwm_frame(&device.mac, &master.mac, device.rx_type, master.channel,
                       index + 1, &pwm);

    loop {
        dongle.send_rf_frame(&rf, device.channel, device.rx_type)?;
        println!("PWM {pwm:?} → {} ({})", mac_str(&device.mac), device.kind.display_name());
        if !hold {
            if percent > 0 {
                println!("note: without --hold, fans revert to hardware default in ~seconds");
            }
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn set_color(index: u8, color: &str) -> Result<()> {
    let rgb = parse_hex_color(color)?;
    let mut dongle = Dongle::open().context("opening dongles")?;
    let master = dongle.discover_master().context("discovering master")?;
    let device = find_device(&mut dongle, index)?;

    let led_count = device.total_leds();
    if led_count == 0 {
        bail!("device reports 0 LEDs — unsupported kind?");
    }
    let frame: Vec<[u8; 3]> = vec![rgb; led_count as usize];
    let fx = dongle.upload_rgb(
        &device.mac, &master.mac, device.channel, device.rx_type,
        &[frame], 5000, 4,
    )?;
    println!(
        "Static #{color} → {} ({}, {} LEDs), effect index {:02x?}",
        mac_str(&device.mac), device.kind.display_name(), led_count, fx,
    );
    Ok(())
}

fn poll_devices(dongle: &mut Dongle) -> Result<llw_protocol::record::GetDevReport> {
    // GetDev can time out sporadically, and the list can be legitimately
    // empty right after a reset — retry both cases before giving up.
    let mut last_empty = None;
    let mut last_err = None;
    for _ in 0..5 {
        match dongle.get_dev() {
            Ok(r) if !r.devices.is_empty() => return Ok(r),
            Ok(r) => last_empty = Some(r),
            Err(e) => last_err = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    if let Some(r) = last_empty {
        return Ok(r); // responsive but no devices — report honestly
    }
    Err(last_err.unwrap().into())
}

fn find_device(dongle: &mut Dongle, index: u8) -> Result<DeviceRecord> {
    let report = poll_devices(dongle)?;
    report
        .devices
        .into_iter()
        .find(|d| d.list_index == index)
        .with_context(|| format!("no device at index {index} — run `llw devices`"))
}

fn parse_hex_color(s: &str) -> Result<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("color must be 6 hex digits, e.g. FF0000");
    }
    Ok([
        u8::from_str_radix(&s[0..2], 16)?,
        u8::from_str_radix(&s[2..4], 16)?,
        u8::from_str_radix(&s[4..6], 16)?,
    ])
}

fn mac_str(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}
```

- [ ] **Step 3: Build and check help output**

```bash
cargo build -p llw-cli 2>&1 | tail -2 && ./target/debug/llw --help
```

Expected: clean build; help lists `scan`, `devices`, `reset`, `set-pwm`, `set-color`.

- [ ] **Step 4: Run the full workspace test suite**

```bash
cargo test 2>&1 | tail -4
```

Expected: all crates pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(cli): llw proof tool (scan/devices/reset/set-pwm/set-color)"
```

---

### Task 10: Hardware validation (manual — M1 acceptance gate)

No files. This task is executed by the owner (or with the owner present) on the real machine. **Do not skip; do not mark M1 complete without it.**

- [ ] **Step 1: Take over the dongles**

```bash
systemctl --user stop lianli-watchdog.service lianli-daemon.service
lsusb | grep 0416   # expect BOTH 8040 (TX) and 8041 (RX) present
```

If `8040` is missing, the TX dongle is in its wedge state — full power-cycle needed before testing (see spec §4.3).

- [ ] **Step 2: Channel survey**

```bash
RUST_LOG=info ./target/debug/llw scan
```

Expected: at least one `channel NN: master ...` line with a nonzero MAC. Note which channels answer (interesting data for M2: is it just one, or several?).

- [ ] **Step 3: Device discovery**

```bash
./target/debug/llw devices
```

Expected: the SL-INF fan cluster(s) and the Strimer listed with sane values — SL-INF shows `fans=N`, nonzero RPMs if spinning; Strimer shows `Strimer Wireless`, `fans=0`. Record the index of each.

- [ ] **Step 4: PWM control (M1 acceptance: "fans spin at commanded PWM")**

```bash
./target/debug/llw set-pwm <SL-INF index> 60 --hold
```

Expected: audible/RPM change within ~2s; `llw devices` in another terminal (stop the hold first — single dongle owner!) or a subsequent run shows `pwm=[153,...]`-ish values (60% ≈ 153). Then Ctrl+C, wait ~30-60s, confirm fans revert to hardware default — that's expected without keepalive and proves why M2 exists.

- [ ] **Step 5: Static color on fans (M1 acceptance: "static color on SL-INF")**

```bash
./target/debug/llw set-color <SL-INF index> FF0000
```

Expected: all fan LEDs turn red and STAY red after the command exits (onboard storage — no host traffic needed).

- [ ] **Step 6: Static color on Strimer (M1 acceptance: "static color on Strimer")**

```bash
./target/debug/llw set-color <Strimer index> 8800FF
```

Expected: the Strimer turns purple and stays purple.

- [ ] **Step 7: Restore the production daemon**

```bash
systemctl --user start lianli-daemon.service lianli-watchdog.service
```

- [ ] **Step 8: Record results**

Append a short "M1 hardware validation — YYYY-MM-DD" section to this plan file: which channels answered, device table output, pass/fail per step, any anomalies (GetDev retry counts, RGB needing a second attempt, etc.). These observations feed M2's acquisition design and M3's animation-size probe. Commit.

---

### Task 11: README, NOTICE, cleanup

**Files:**
- Create: `README.md`
- Create: `NOTICE`

- [ ] **Step 1: Create `NOTICE`**

```text
lian-li-wireless
Copyright (c) 2026 Morgan Blem

Portions of crates/llw-protocol (protocol constants, frame layouts, device
record parsing, USB transport, tinyuz FFI wrapper and build integration) are
ported from lian-li-linux (https://github.com/sgtaziz/lian-li-linux),
Copyright (c) 2026 sgtaziz, MIT License.

vendor/tuz_wrapper.cpp is copied from the same project.
vendor/tinyuz and vendor/HDiffPatch are upstream libraries by sisong
(https://github.com/sisong), MIT License, vendored as git submodules.
```

- [ ] **Step 2: Create `README.md`**

```markdown
# lian-li-wireless

Linux support for Lian Li's 2.4GHz wireless ecosystem — UNI FAN wireless
(SL-INF, SL V3, TL, CL), Strimer Wireless, and wireless AIOs — with a focus
on rock-solid fan control and full dynamic RGB.

**Status: M1 — protocol library + proof CLI.** Not yet a daemon; see
`docs/superpowers/specs/` for the design and roadmap (reliability daemon,
effect engine, Tauri UI).

## Requirements

- A Lian Li wireless TX/RX dongle pair (V1 `0416:8040/8041` or V2
  `1A86:E304/E305`) with devices already bound (bind via L-Connect or
  lian-li-linux for now; native binding lands with the UI milestone).
- udev permissions on the dongles (the lian-li-linux package's rules work;
  standalone rules ship with the packaging milestone).
- Stop any other software that owns the dongles (lianli-daemon, etc.) —
  only one process can drive them.

## Build & try

    git clone --recurse-submodules <repo-url>
    cargo build --release
    ./target/release/llw scan       # find the master dongle's RF channel
    ./target/release/llw devices    # list bound wireless devices
    ./target/release/llw set-pwm 0 60 --hold
    ./target/release/llw set-color 1 FF0000

## License

MIT. Protocol knowledge ported from
[sgtaziz/lian-li-linux](https://github.com/sgtaziz/lian-li-linux) (MIT) —
see NOTICE.
```

- [ ] **Step 3: Add MIT `LICENSE` file**

Standard MIT text, `Copyright (c) 2026 Morgan Blem`.

- [ ] **Step 4: Remove the upstream reference worktree**

```bash
git -C /home/morganblem/lian-li-linux-pr worktree remove /home/morganblem/lian-li-wireless/.upstream
```

- [ ] **Step 5: Final check + commit**

```bash
cargo test 2>&1 | tail -3 && cargo clippy --workspace 2>&1 | tail -3
git add -A && git commit -m "docs: README, NOTICE, LICENSE"
```

Expected: tests pass; clippy warnings addressed or consciously accepted (note them in the commit body if kept).

---

## Self-review notes (already applied)

- **Spec coverage (M1 slice):** §3.1 protocol library → Tasks 2-8; CLI proof → Task 9; M1 acceptance criteria → Task 10 steps 4-6 map 1:1. Deliberately absent (later milestones): bind/unbind (M4 Devices screen), master-clock sending loop + keepalive + recovery tiers (M2), effect rendering (M3), `send_rgb_direct` streaming path (post-v1; `rgb_frames` with 1 frame covers static).
- **Types:** `DeviceKind` (Task 4) is referenced by `record.rs` (Task 5), `frames.rs` (Task 6), CLI (Task 9) — names match. `GetDevReport`/`DeviceRecord` consistent across Tasks 5, 8, 9. `RfFrame` alias defined Task 6, used Task 8.
- **Known judgment calls:** the FNV-1a "abc" test vector includes an independent-verification one-liner in case the constant was transcribed wrong; `poll_devices` retries 5× because GetDev timeouts are empirically sporadic; `seq_index = index + 1` in `set-pwm` mirrors upstream's position-among-bound-devices semantics and is correct for single-master setups.

---

## M1 hardware validation — 2026-07-13

Environment: owner's machine, V1 dongles (TX `0416:8040` bus7, RX `0416:8041` bus7), `lianli-daemon` + watchdog stopped for the session and restarted cleanly afterwards. Binary: `target/release/llw` @ ae14f8c.

| Step | Result |
|------|--------|
| 1. Dongle takeover | PASS — both dongles enumerated, no TX wedge |
| 2. `llw scan` | PASS — master `e5:ba:f0:72:ab:3c` fw=16 answered on **every channel 2–39** (only ch 1 silent); ~20s survey |
| 3. `llw devices` | PASS — SL-INF cluster `02:8b:51:62:32:e1` (3 fans, ch=2, rx=1) with live RPM/PWM/fx telemetry. Strimer absent (see note) |
| 4. `llw set-pwm 0 60 --hold` (20s) | PASS — raw 153 to slots [153,153,153,0]; RPM fell 2193→~1700, audibly confirmed; reverted to hardware default (~2200) after hold ended, as expected without keepalive |
| 5. `llw set-color 0 FF0000` | PASS — 132 LEDs, 396B raw → 13B compressed, 2 RF frames; all three rings solid steady red; firmware echoed effect index `af:c0:e0:21` in GetDev **6s after zero traffic** = onboard storage confirmed |
| 6. Strimer color | **DEFERRED** — Strimer Wireless owned but not yet physically installed; absence from discovery is correct behavior. The code path is identical to step 5 (same `upload_rgb`); run `llw devices` + `llw set-color <idx> 8800FF` once installed to close this out |
| 7. Daemon restore | PASS — daemon reacquired dongles, resumed PWM 86 within 8s |

### Anomalies / M2-relevant observations

- **GET_MAC answers on all channels (2–39).** The master responds to the channel-scan query on essentially every channel, so "first responder" carries no information — upstream's ordering (8 first) alone explains the boot-time channel-8 lock-in. M2's scored acquisition (response-quality bursts per candidate, spec §4.1) is therefore not just better but *necessary*; mere GET_MAC response cannot discriminate channels. Channel 1's silence is a curiosity worth re-checking during M2.
- PWM readback dropped to `[0,0,0,0]` within seconds of the hold ending — the documented no-keepalive dropout, reconfirmed; M2's 1s keepalive + drift detection owns this.
- GetDev polls were reliable throughout (no retries observed in any invocation this session).

**M1 acceptance: PASSED** (fan PWM + fan static color on real hardware; Strimer deferred pending physical install — protocol-identical path validated).
