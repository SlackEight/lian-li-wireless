# M2a: Daemon Foundations — Implementation Plan (M2 part 1 of 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every testable building block of the M2 reliability daemon — a transport seam for hardware-free simulation, the `llw-daemon` crate with config/migration, native hwmon sensors, curve+hysteresis math, keepalive policy, the tiered-recovery state machine, static-RGB assertion — plus the `llw watch` diagnostic and the channel-behavior experiment whose findings gate M2b's acquisition design.

**Architecture:** Same discipline as M1: everything that can be pure is pure and unit-tested (curve math, hysteresis, keepalive decisions, tier transitions with injected timestamps, RGB frame building); I/O stays at the edges. The daemon's supervisor loop, IPC server, and packaging are **M2b** (a separate plan, written after Task 10's experiment findings). Each task here yields working, tested software.

**Tech Stack:** additions to the workspace — `serde`/`serde_json` (config), `tempfile` (dev, fake sysfs trees). No new runtime services yet.

**Context for the engineer:**
- Spec: `docs/superpowers/specs/2026-07-13-lian-li-wireless-design.md` (§3.3 daemon responsibilities, §4 reliability model, §8 testing).
- M1 validation results (bottom of `docs/superpowers/plans/2026-07-13-m1-protocol-port-cli.md`) — especially: GET_MAC answers on ALL channels 2–39, so first-responder acquisition carries no information.
- Upstream reference values (verified against `sgtaziz/lian-li-linux` @ d262007 during recon; no upstream checkout needed for this plan — all needed logic is restated below with values):
  - Curve: sort points by temp ascending; linear interpolation; below-min→min speed, above-max→max speed; empty/single-point→50%.
  - Speed%→PWM: `(pct * 2.55) as u8`. Observed live: 34% → PWM 86 on the owner's machine.
  - Temp smoothing: EMA with α=0.3; readings outside 0–110 °C are ignored (keep previous).
  - Hysteresis: hold last PWM only when `pwm_delta < hysteresis_pwm` AND `temp_delta < hysteresis_temp` (defaults 5 / 1.0 °C).
  - Keepalive send rule (from upstream `fan_speed.rs`, policy now ours): send when any slot has `|desired − readback| > 5`, or (`desired ≤ 10` and `readback ≠ desired`), or ≥1s since last send.
  - RGB brightness: `channel_byte × (brightness / 4.0).clamp(0.0, 1.0)`, brightness 0–4.
- The owner's live config to migrate (`~/.config/lianli/config.json`): curve-1 = k10temp via shell command, points `[[29,30],[52,34],[69,35],[89,37],[40,34],[78,35]]` (unsorted!), 3 slots on curve-1 + slot 4 = 0, `update_interval_ms: 200`, `hysteresis_temp: 1.0`, `hysteresis_pwm: 5`, RGB zone 0 = Direct white `[255,255,255]` brightness 4.
- **Hardware rules:** Tasks 1–9 must NOT touch the dongles (production daemon owns them). Task 10 is manual, with the owner present, same stop/start ritual as M1's Task 10.
- Work directly on `main`.

---

## File structure (end state of M2a)

```
crates/
├── llw-protocol/src/
│   └── io.rs                    # NEW: UsbIo trait + FakeIo; dongle.rs becomes generic
├── llw-daemon/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs              # stub: --import-lianli + config check (supervisor is M2b)
│       ├── config.rs            # schema v1, load/save
│       ├── migrate.rs           # import from ~/.config/lianli/config.json
│       ├── sensors.rs           # hwmon resolver + EMA
│       ├── curve.rs             # interpolation + hysteresis (pure)
│       ├── fan.rs               # slot resolution + keepalive policy (pure)
│       ├── reliability.rs       # tiered-recovery state machine (pure, injected time)
│       └── rgb_assert.rs        # static color → frame + drift compare (pure)
└── llw-cli/src/main.rs          # + `llw watch [--interval-ms N] [--pwm P]`
```

`main.rs` stays a stub until M2b wires the supervisor; the crate still ships value now (importer + config validation), and every module below it is fully tested.

---

### Task 1: Transport seam (`UsbIo` trait + generic `Dongle` + `FakeIo`)

**Files:**
- Create: `crates/llw-protocol/src/io.rs`
- Modify: `crates/llw-protocol/src/dongle.rs` (make `Dongle` generic)
- Modify: `crates/llw-protocol/src/lib.rs` (add `pub mod io;`)

- [ ] **Step 1: Create `io.rs`**

```rust
//! I/O abstraction over the USB transport, enabling hardware-free tests.

use crate::Result;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

/// The three operations `Dongle` needs from a transport.
pub trait UsbIo {
    fn write(&self, data: &[u8], timeout: Duration) -> Result<usize>;
    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize>;
    fn read_flush(&self);
}

impl UsbIo for crate::transport::UsbTransport {
    fn write(&self, data: &[u8], timeout: Duration) -> Result<usize> {
        crate::transport::UsbTransport::write(self, data, timeout)
    }
    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        crate::transport::UsbTransport::read(self, buf, timeout)
    }
    fn read_flush(&self) {
        crate::transport::UsbTransport::read_flush(self)
    }
}

/// Scripted in-memory transport for tests and simulations.
/// Writes are recorded; reads pop from a script queue (empty queue = timeout,
/// matching real-dongle silence).
#[derive(Default)]
pub struct FakeIo {
    pub writes: Mutex<Vec<Vec<u8>>>,
    pub reads: Mutex<VecDeque<Result<Vec<u8>>>>,
}

impl FakeIo {
    pub fn push_read(&self, data: Vec<u8>) {
        self.reads.lock().unwrap().push_back(Ok(data));
    }
    pub fn push_read_err(&self, err: crate::ProtocolError) {
        self.reads.lock().unwrap().push_back(Err(err));
    }
    pub fn written(&self) -> Vec<Vec<u8>> {
        self.writes.lock().unwrap().clone()
    }
}

impl UsbIo for FakeIo {
    fn write(&self, data: &[u8], _timeout: Duration) -> Result<usize> {
        self.writes.lock().unwrap().push(data.to_vec());
        Ok(data.len())
    }
    fn read(&self, buf: &mut [u8], _timeout: Duration) -> Result<usize> {
        match self.reads.lock().unwrap().pop_front() {
            Some(Ok(data)) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                Ok(n)
            }
            Some(Err(e)) => Err(e),
            None => Err(crate::ProtocolError::Usb(rusb::Error::Timeout)),
        }
    }
    fn read_flush(&self) {}
}
```

- [ ] **Step 2: Make `Dongle` generic in `dongle.rs`**

Mechanical refactor — behavior identical:

1. `use crate::io::UsbIo;` and `use crate::transport::UsbTransport;` (adjust existing imports).
2. Struct becomes:

```rust
pub struct Dongle<T: UsbIo = UsbTransport> {
    tx: T,
    rx: Option<T>,
}
```

3. Split the impl blocks: `open()` (and only `open`) stays under `impl Dongle<UsbTransport>` — it constructs real transports and calls `detach_and_configure`. Everything else (`has_rx`, `reset`, `get_mac`, `survey_channels`, `discover_master`, `get_dev`, `send_rf_frame`, `upload_rgb`) moves under `impl<T: UsbIo> Dongle<T>` unchanged.
4. Add to the generic impl:

```rust
    /// Assemble a Dongle from raw parts (tests/simulations).
    pub fn from_parts(tx: T, rx: Option<T>) -> Self {
        Self { tx, rx }
    }
```

5. `open_any` keeps returning `UsbTransport` (only `open` uses it).

- [ ] **Step 3: Register `pub mod io;` in lib.rs (alphabetical: consts, device_kind, dongle, frames, io, record, tinyuz, transport)**

- [ ] **Step 4: Add fake-driven tests to `dongle.rs`'s tests module**

```rust
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
```

Note: `d.tx` is a private field — these tests live in `dongle.rs`'s own tests module, so access is fine.

- [ ] **Step 5: Run everything**

```bash
cargo test -p llw-protocol --lib 2>&1 | tail -3   # 36 passed (32 + 4 new)
cargo build -p llw-cli 2>&1 | tail -2              # CLI unaffected (default type param)
cargo clippy --workspace --all-targets 2>&1 | tail -3   # zero warnings
```

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(protocol): UsbIo transport seam + FakeIo, Dongle made generic"
```

---

### Task 2: `llw-daemon` crate + config schema v1

**Files:**
- Create: `crates/llw-daemon/Cargo.toml`
- Create: `crates/llw-daemon/src/main.rs`
- Create: `crates/llw-daemon/src/config.rs`
- Modify: root `Cargo.toml` ([workspace.dependencies]: add `serde`, `serde_json`, `tempfile`)

- [ ] **Step 1: Workspace deps** — add to root `Cargo.toml` `[workspace.dependencies]`:

```toml
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tempfile = "3.10"
```

- [ ] **Step 2: `crates/llw-daemon/Cargo.toml`**

```toml
[package]
name = "llw-daemon"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Reliability daemon for Lian Li wireless devices (fans, RGB, link supervision)"

[dependencies]
llw-protocol = { path = "../llw-protocol" }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 3: `src/config.rs` (complete file)**

```rust
//! Versioned daemon configuration (schema v1).
//! Path: ~/.config/lian-li-wireless/config.json

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub curves: Vec<Curve>,
    #[serde(default)]
    pub devices: Vec<DeviceConfig>,
    #[serde(default)]
    pub control: ControlConfig,
    #[serde(default)]
    pub reliability: ReliabilityConfig,
}

/// A named temp→speed curve bound to a hwmon sensor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Curve {
    pub name: String,
    pub sensor: SensorSpec,
    /// (temp °C, speed %) points; stored order is irrelevant (sorted on load).
    pub points: Vec<(f32, f32)>,
}

/// Native hwmon addressing: /sys/class/hwmon/hwmon*/name == `hwmon_name`,
/// reading `input` (e.g. "temp1_input", millidegrees).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorSpec {
    pub hwmon_name: String,
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// Device MAC as "aa:bb:cc:dd:ee:ff".
    pub mac: String,
    #[serde(default)]
    pub name: Option<String>,
    /// One entry per fan slot. RGB-only devices use an empty array.
    pub slots: [SlotSpeed; 4],
    /// Static color asserted (and drift-restored) by the daemon. None = leave alone.
    #[serde(default)]
    pub color: Option<StaticColor>,
}

/// Untagged: a number is a constant speed %, a string names a curve. 0 = off.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SlotSpeed {
    Percent(u8),
    Curve(String),
}

impl Default for SlotSpeed {
    fn default() -> Self {
        SlotSpeed::Percent(0)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct StaticColor {
    pub rgb: [u8; 3],
    /// 0..=4, L-Connect-compatible scale (4 = full).
    pub brightness: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlConfig {
    /// Fan control tick in ms.
    pub tick_ms: u64,
    pub hysteresis_temp: f32,
    pub hysteresis_pwm: u8,
    /// PWM keepalive interval in ms (firmware reverts without traffic).
    pub keepalive_ms: u64,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            tick_ms: 1000,
            hysteresis_temp: 1.0,
            hysteresis_pwm: 5,
            keepalive_ms: 1000,
        }
    }
}

/// Spec §4.2 thresholds — tuning parameters, not constants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReliabilityConfig {
    pub grace_s: u64,
    pub dropout_threshold: u32,
    pub window_s: u64,
    pub tier1_cooldown_s: u64,
    pub tier2_cooldown_s: u64,
    pub tier2_after_failed_tier1: u32,
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            grace_s: 120,
            dropout_threshold: 5,
            window_s: 60,
            tier1_cooldown_s: 60,
            tier2_cooldown_s: 300,
            tier2_after_failed_tier1: 2,
        }
    }
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("lian-li-wireless").join("config.json")
}

impl Config {
    pub fn new() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            ..Default::default()
        }
    }

    /// Load from `path`. Missing file → default config (not an error).
    /// Wrong schema_version → hard error (migrations are explicit).
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if cfg.schema_version != SCHEMA_VERSION {
            bail!(
                "config schema_version {} unsupported (daemon supports {})",
                cfg.schema_version,
                SCHEMA_VERSION
            );
        }
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Referential integrity: every named curve exists; brightness in range;
    /// MACs parseable.
    pub fn validate(&self) -> Result<()> {
        for dev in &self.devices {
            parse_mac(&dev.mac).with_context(|| format!("device mac {:?}", dev.mac))?;
            for slot in &dev.slots {
                if let SlotSpeed::Curve(name) = slot {
                    if !self.curves.iter().any(|c| &c.name == name) {
                        bail!("device {} references unknown curve {:?}", dev.mac, name);
                    }
                }
            }
            if let Some(c) = &dev.color {
                if c.brightness > 4 {
                    bail!("device {} brightness {} out of range 0-4", dev.mac, c.brightness);
                }
            }
        }
        Ok(())
    }
}

pub fn parse_mac(s: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        bail!("expected 6 colon-separated octets");
    }
    let mut mac = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(p, 16).with_context(|| format!("octet {:?}", p))?;
    }
    Ok(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        let mut cfg = Config::new();
        cfg.curves.push(Curve {
            name: "cpu".into(),
            sensor: SensorSpec {
                hwmon_name: "k10temp".into(),
                input: "temp1_input".into(),
            },
            points: vec![(29.0, 30.0), (89.0, 37.0)],
        });
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: Some("top fans".into()),
            slots: [
                SlotSpeed::Curve("cpu".into()),
                SlotSpeed::Curve("cpu".into()),
                SlotSpeed::Curve("cpu".into()),
                SlotSpeed::Percent(0),
            ],
            color: Some(StaticColor { rgb: [255, 255, 255], brightness: 4 }),
        });
        cfg
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = sample();
        cfg.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.curves.len(), 1);
        assert_eq!(loaded.devices[0].slots[0], SlotSpeed::Curve("cpu".into()));
        assert_eq!(
            loaded.devices[0].color,
            Some(StaticColor { rgb: [255, 255, 255], brightness: 4 })
        );
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&dir.path().join("nope.json")).unwrap();
        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
        assert!(cfg.devices.is_empty());
    }

    #[test]
    fn wrong_schema_version_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"schema_version": 99}"#).unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn unknown_curve_reference_rejected() {
        let mut cfg = sample();
        cfg.curves.clear();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn slot_speed_untagged_shapes() {
        let s: SlotSpeed = serde_json::from_str("40").unwrap();
        assert_eq!(s, SlotSpeed::Percent(40));
        let s: SlotSpeed = serde_json::from_str(r#""cpu""#).unwrap();
        assert_eq!(s, SlotSpeed::Curve("cpu".into()));
    }

    #[test]
    fn parses_mac() {
        assert_eq!(
            parse_mac("02:8b:51:62:32:e1").unwrap(),
            [0x02, 0x8b, 0x51, 0x62, 0x32, 0xe1]
        );
        assert!(parse_mac("02:8b:51").is_err());
        assert!(parse_mac("zz:8b:51:62:32:e1").is_err());
    }
}
```

- [ ] **Step 4: `src/main.rs` (stub — supervisor arrives in M2b)**

```rust
//! llw-daemon — reliability daemon for Lian Li wireless devices.
//! M2a stub: config tooling only. The supervisor loop lands in M2b.

mod config;

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--check-config") => {
            let path = config::default_path();
            let cfg = config::Config::load(&path)?;
            println!(
                "OK: {} ({} curve(s), {} device(s))",
                path.display(),
                cfg.curves.len(),
                cfg.devices.len()
            );
            Ok(())
        }
        _ => {
            eprintln!("llw-daemon (M2a): supervisor not yet implemented.");
            eprintln!("usage: llw-daemon --check-config");
            std::process::exit(2);
        }
    }
}
```

- [ ] **Step 5: Test + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3    # 6 passed
cargo run -p llw-daemon -- --check-config  # OK: ... (default config, 0 curves)
git add -A && git commit -m "feat(daemon): crate skeleton + versioned config schema v1"
```

---

### Task 3: Migration importer (`migrate.rs`)

**Files:**
- Create: `crates/llw-daemon/src/migrate.rs`
- Modify: `crates/llw-daemon/src/main.rs` (add `--import-lianli` command + `mod migrate;`)

- [ ] **Step 1: Write `migrate.rs` (complete file)**

```rust
//! One-shot import from lianli-daemon's config (~/.config/lianli/config.json).
//! Loose parsing on purpose: we read a foreign schema and take what we support.

use crate::config::{
    Config, Curve, DeviceConfig, SensorSpec, SlotSpeed, StaticColor, SCHEMA_VERSION,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

pub struct ImportReport {
    pub config: Config,
    pub warnings: Vec<String>,
}

pub fn import(path: &Path) -> Result<ImportReport> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let v: Value = serde_json::from_str(&text).context("parsing lianli config")?;
    Ok(import_value(&v))
}

pub fn import_value(v: &Value) -> ImportReport {
    let mut warnings = Vec::new();
    let mut cfg = Config::new();
    cfg.schema_version = SCHEMA_VERSION;

    // Curves: keep name + points; map temp_command → native hwmon by best effort.
    for c in v["fan_curves"].as_array().unwrap_or(&Vec::new()) {
        let name = c["name"].as_str().unwrap_or("imported").to_string();
        let points: Vec<(f32, f32)> = c["curve"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|p| {
                let pair = p.as_array()?;
                Some((pair.first()?.as_f64()? as f32, pair.get(1)?.as_f64()? as f32))
            })
            .collect();
        let cmd = c["temp_command"].as_str().unwrap_or("");
        let sensor = sensor_from_temp_command(cmd, &mut warnings, &name);
        cfg.curves.push(Curve { name, sensor, points });
    }

    // Fan groups: only wireless:<mac> device ids are ours.
    for g in v["fans"]["speeds"].as_array().unwrap_or(&Vec::new()) {
        let Some(device_id) = g["device_id"].as_str() else { continue };
        let Some(mac) = device_id.strip_prefix("wireless:") else {
            warnings.push(format!("skipping non-wireless device {device_id:?}"));
            continue;
        };
        let mut slots = [
            SlotSpeed::Percent(0),
            SlotSpeed::Percent(0),
            SlotSpeed::Percent(0),
            SlotSpeed::Percent(0),
        ];
        for (i, s) in g["speeds"].as_array().unwrap_or(&Vec::new()).iter().enumerate() {
            if i >= 4 {
                break;
            }
            slots[i] = match s {
                Value::String(name) if name.starts_with("__mb_sync__") => {
                    warnings.push(format!(
                        "slot {i} of {mac}: motherboard-sync not supported yet, set to 0"
                    ));
                    SlotSpeed::Percent(0)
                }
                Value::String(name) => SlotSpeed::Curve(name.clone()),
                Value::Number(n) => SlotSpeed::Percent(n.as_u64().unwrap_or(0).min(100) as u8),
                _ => SlotSpeed::Percent(0),
            };
        }
        let color = extract_static_color(v, device_id, &mut warnings);
        cfg.devices.push(DeviceConfig { mac: mac.to_string(), name: None, slots, color });
    }

    // Control parameters.
    if let Some(ms) = v["fans"]["update_interval_ms"].as_u64() {
        cfg.control.tick_ms = ms;
    }
    if let Some(t) = v["fans"]["hysteresis_temp"].as_f64() {
        cfg.control.hysteresis_temp = t as f32;
    }
    if let Some(p) = v["fans"]["hysteresis_pwm"].as_u64() {
        cfg.control.hysteresis_pwm = p as u8;
    }

    ImportReport { config: cfg, warnings }
}

/// lianli-daemon runs shell commands for temps; we address hwmon natively.
/// Recognize the common "find hwmon by name X, read temp1_input" shape.
fn sensor_from_temp_command(cmd: &str, warnings: &mut Vec<String>, curve: &str) -> SensorSpec {
    for known in ["k10temp", "coretemp", "zenpower"] {
        if cmd.contains(known) {
            let input = if cmd.contains("temp2_input") {
                "temp2_input"
            } else {
                "temp1_input"
            };
            return SensorSpec { hwmon_name: known.into(), input: input.into() };
        }
    }
    warnings.push(format!(
        "curve {curve:?}: could not map temp_command to a hwmon sensor; defaulting to k10temp/temp1_input — VERIFY"
    ));
    SensorSpec { hwmon_name: "k10temp".into(), input: "temp1_input".into() }
}

/// First zone of the matching rgb device, Direct/Static single color only.
fn extract_static_color(v: &Value, device_id: &str, warnings: &mut Vec<String>) -> Option<StaticColor> {
    let devices = v["rgb"]["devices"].as_array()?;
    let dev = devices.iter().find(|d| d["device_id"].as_str() == Some(device_id))?;
    let zone = dev["zones"].as_array()?.first()?;
    let effect = &zone["effect"];
    let mode = effect["mode"].as_str().unwrap_or("");
    if mode != "Direct" && mode != "Static" {
        warnings.push(format!(
            "{device_id}: RGB mode {mode:?} is not a static color; skipping color import (effects arrive in M3)"
        ));
        return None;
    }
    let c = effect["colors"].as_array()?.first()?.as_array()?;
    let rgb = [
        c.first()?.as_u64()? as u8,
        c.get(1)?.as_u64()? as u8,
        c.get(2)?.as_u64()? as u8,
    ];
    let brightness = effect["brightness"].as_u64().unwrap_or(4).min(4) as u8;
    Some(StaticColor { rgb, brightness })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape-faithful excerpt of the owner's real lianli config.
    const LIANLI: &str = r#"{
      "fan_curves": [{
        "name": "curve-1",
        "temp_command": "for h in /sys/class/hwmon/hwmon*; do if [ \"$(cat \"$h/name\" 2>/dev/null)\" = k10temp ]; then awk '{print $1/1000}' \"$h/temp1_input\"; exit; fi; done",
        "curve": [[29.0,30.0],[52.0,34.0],[69.0,35.0],[89.0,37.0],[40.0,34.0],[78.0,35.0]]
      }],
      "fans": {
        "speeds": [{"device_id": "wireless:02:8b:51:62:32:e1", "speeds": ["curve-1","curve-1","curve-1",0]}],
        "update_interval_ms": 200, "hysteresis_temp": 1.0, "hysteresis_pwm": 5
      },
      "rgb": {"enabled": true, "devices": [{
        "device_id": "wireless:02:8b:51:62:32:e1",
        "zones": [{"zone_index": 0, "effect": {"mode": "Direct", "colors": [[255,255,255]], "speed": 2, "brightness": 4}}]
      }]}
    }"#;

    #[test]
    fn imports_owner_config() {
        let v: Value = serde_json::from_str(LIANLI).unwrap();
        let report = import_value(&v);
        let cfg = &report.config;

        assert_eq!(cfg.curves.len(), 1);
        assert_eq!(cfg.curves[0].name, "curve-1");
        assert_eq!(cfg.curves[0].sensor.hwmon_name, "k10temp");
        assert_eq!(cfg.curves[0].sensor.input, "temp1_input");
        assert_eq!(cfg.curves[0].points.len(), 6);

        assert_eq!(cfg.devices.len(), 1);
        let dev = &cfg.devices[0];
        assert_eq!(dev.mac, "02:8b:51:62:32:e1");
        assert_eq!(dev.slots[0], SlotSpeed::Curve("curve-1".into()));
        assert_eq!(dev.slots[3], SlotSpeed::Percent(0));
        assert_eq!(dev.color, Some(StaticColor { rgb: [255, 255, 255], brightness: 4 }));

        assert_eq!(cfg.control.tick_ms, 200);
        assert_eq!(cfg.control.hysteresis_pwm, 5);
        assert!(cfg.validate().is_ok());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn unmappable_sensor_warns_and_defaults() {
        let v: Value = serde_json::from_str(
            r#"{"fan_curves":[{"name":"x","temp_command":"cat /weird","curve":[[20,30]]}],
                "fans":{"speeds":[]}}"#,
        )
        .unwrap();
        let report = import_value(&v);
        assert_eq!(report.config.curves[0].sensor.hwmon_name, "k10temp");
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn non_static_rgb_mode_is_skipped_with_warning() {
        let v: Value = serde_json::from_str(
            r#"{"fans":{"speeds":[{"device_id":"wireless:aa:bb:cc:dd:ee:ff","speeds":[0,0,0,0]}]},
                "rgb":{"devices":[{"device_id":"wireless:aa:bb:cc:dd:ee:ff",
                  "zones":[{"effect":{"mode":"Rainbow","colors":[[1,2,3]]}}]}]}}"#,
        )
        .unwrap();
        let report = import_value(&v);
        assert_eq!(report.config.devices[0].color, None);
        assert!(report.warnings.iter().any(|w| w.contains("Rainbow")));
    }

    #[test]
    fn wired_devices_are_skipped() {
        let v: Value = serde_json::from_str(
            r#"{"fans":{"speeds":[{"device_id":"hid:1234","speeds":[50,50,50,50]}]}}"#,
        )
        .unwrap();
        let report = import_value(&v);
        assert!(report.config.devices.is_empty());
        assert_eq!(report.warnings.len(), 1);
    }
}
```

- [ ] **Step 2: Wire `--import-lianli` into `main.rs`** — add `mod migrate;` and this match arm before the fallthrough:

```rust
        Some("--import-lianli") => {
            let src = args.get(2).map(std::path::PathBuf::from).unwrap_or_else(|| {
                config::default_path()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("lianli")
                    .join("config.json")
            });
            let dst = config::default_path();
            if dst.exists() && args.iter().all(|a| a != "--force") {
                anyhow::bail!("{} already exists (use --force to overwrite)", dst.display());
            }
            let report = migrate::import(&src)?;
            for w in &report.warnings {
                eprintln!("warning: {w}");
            }
            report.config.save(&dst)?;
            println!(
                "Imported {} curve(s), {} device(s) → {}",
                report.config.curves.len(),
                report.config.devices.len(),
                dst.display()
            );
            Ok(())
        }
```

Also update the usage line: `eprintln!("usage: llw-daemon --check-config | --import-lianli [path] [--force]");`

Also: `Config::save` gains its first caller here — REMOVE the temporary `#[allow(dead_code)]` on `save()` in config.rs (added during Task 2 because llw-daemon is a bin crate and the fn was uncalled until now).

- [ ] **Step 3: Test + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 11 passed (10 + Task 2's review-added percent test)
git add -A && git commit -m "feat(daemon): lianli config importer (curves, slots, static color)"
```

---

### Task 4: hwmon sensors (`sensors.rs`)

**Files:**
- Create: `crates/llw-daemon/src/sensors.rs`
- Modify: `crates/llw-daemon/src/main.rs` (add `mod sensors;`)

- [ ] **Step 1: Write `sensors.rs` (complete file)**

```rust
//! Native hwmon temperature reading (replaces lianli-daemon's shell commands).

use crate::config::SensorSpec;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub struct HwmonSensor {
    input_path: PathBuf,
}

/// Resolve a SensorSpec against a hwmon tree root (production: /sys/class/hwmon).
pub fn resolve(base: &Path, spec: &SensorSpec) -> Result<HwmonSensor> {
    let entries = std::fs::read_dir(base)
        .with_context(|| format!("reading hwmon dir {}", base.display()))?;
    for entry in entries.flatten() {
        let name_path = entry.path().join("name");
        let Ok(name) = std::fs::read_to_string(&name_path) else { continue };
        if name.trim() == spec.hwmon_name {
            let input_path = entry.path().join(&spec.input);
            if !input_path.exists() {
                bail!(
                    "hwmon {:?} found but has no {:?}",
                    spec.hwmon_name,
                    spec.input
                );
            }
            return Ok(HwmonSensor { input_path });
        }
    }
    bail!("no hwmon named {:?} under {}", spec.hwmon_name, base.display())
}

impl HwmonSensor {
    /// Read the temperature in °C (sysfs reports millidegrees).
    pub fn read_c(&self) -> Result<f32> {
        let raw = std::fs::read_to_string(&self.input_path)
            .with_context(|| format!("reading {}", self.input_path.display()))?;
        let milli: f32 = raw.trim().parse().context("parsing millidegrees")?;
        Ok(milli / 1000.0)
    }
}

/// Exponential moving average with plausibility gating (upstream α = 0.3;
/// readings outside 0–110 °C keep the previous value).
pub struct Ema {
    alpha: f32,
    value: Option<f32>,
}

impl Ema {
    pub fn new(alpha: f32) -> Self {
        Self { alpha, value: None }
    }

    pub fn update(&mut self, reading: f32) -> Option<f32> {
        if (0.0..=110.0).contains(&reading) {
            self.value = Some(match self.value {
                Some(prev) => self.alpha * reading + (1.0 - self.alpha) * prev,
                None => reading,
            });
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_hwmon(dir: &Path, index: u32, name: &str, temp_milli: i32) {
        let d = dir.join(format!("hwmon{index}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("name"), format!("{name}\n")).unwrap();
        std::fs::write(d.join("temp1_input"), format!("{temp_milli}\n")).unwrap();
    }

    fn spec(name: &str) -> SensorSpec {
        SensorSpec { hwmon_name: name.into(), input: "temp1_input".into() }
    }

    #[test]
    fn resolves_by_name_and_reads_millidegrees() {
        let dir = tempfile::tempdir().unwrap();
        fake_hwmon(dir.path(), 0, "nvme", 35_000);
        fake_hwmon(dir.path(), 3, "k10temp", 41_250);
        let s = resolve(dir.path(), &spec("k10temp")).unwrap();
        assert!((s.read_c().unwrap() - 41.25).abs() < 0.001);
    }

    #[test]
    fn missing_name_or_input_errors() {
        let dir = tempfile::tempdir().unwrap();
        fake_hwmon(dir.path(), 0, "nvme", 35_000);
        assert!(resolve(dir.path(), &spec("k10temp")).is_err());
        let bad = SensorSpec { hwmon_name: "nvme".into(), input: "temp9_input".into() };
        assert!(resolve(dir.path(), &bad).is_err());
    }

    #[test]
    fn ema_smooths_and_gates() {
        let mut ema = Ema::new(0.3);
        assert_eq!(ema.update(40.0), Some(40.0)); // first reading adopted
        let v = ema.update(50.0).unwrap(); // 0.3*50 + 0.7*40 = 43
        assert!((v - 43.0).abs() < 0.001);
        // implausible readings keep the previous value
        assert!((ema.update(-5.0).unwrap() - 43.0).abs() < 0.001);
        assert!((ema.update(400.0).unwrap() - 43.0).abs() < 0.001);
    }
}
```

- [ ] **Step 2: Register module, test, commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 14 passed
git add -A && git commit -m "feat(daemon): native hwmon sensor resolution + EMA smoothing"
```

---

### Task 5: Curve math (`curve.rs`)

**Files:**
- Create: `crates/llw-daemon/src/curve.rs`
- Modify: `crates/llw-daemon/src/main.rs` (add `mod curve;`)

- [ ] **Step 1: Write `curve.rs` (complete file)**

```rust
//! Temp→speed curve interpolation and PWM hysteresis (pure).
//! Semantics match lianli-daemon (validated live: 34% → PWM 86).

/// A curve with points sorted by temperature (sorted once at construction —
/// upstream re-sorts every evaluation).
pub struct SortedCurve {
    points: Vec<(f32, f32)>,
}

impl SortedCurve {
    pub fn new(mut points: Vec<(f32, f32)>) -> Self {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { points }
    }

    /// Speed % for a temperature. Empty/single-point curves → 50 / the point's
    /// speed; below min → min speed; above max → max speed; else linear.
    pub fn eval(&self, temp: f32) -> f32 {
        match self.points.len() {
            0 => return 50.0,
            1 => return self.points[0].1,
            _ => {}
        }
        let first = self.points[0];
        let last = *self.points.last().unwrap();
        if temp <= first.0 {
            return first.1;
        }
        if temp >= last.0 {
            return last.1;
        }
        for w in self.points.windows(2) {
            let (t1, s1) = w[0];
            let (t2, s2) = w[1];
            if temp >= t1 && temp <= t2 {
                if (t2 - t1).abs() < f32::EPSILON {
                    return s1;
                }
                let ratio = (temp - t1) / (t2 - t1);
                return s1 + ratio * (s2 - s1);
            }
        }
        last.1
    }
}

/// Speed % → PWM byte (upstream: `(pct * 2.55) as u8`).
pub fn percent_to_pwm(pct: f32) -> u8 {
    (pct * 2.55) as u8
}

/// Hold the last PWM while BOTH the PWM delta and the temp delta are below
/// their thresholds (prevents chatter around curve breakpoints).
#[derive(Default)]
pub struct Hysteresis {
    last_temp: Option<f32>,
    last_pwm: Option<u8>,
}

impl Hysteresis {
    pub fn apply(&mut self, temp: f32, target_pwm: u8, ht: f32, hp: u8) -> u8 {
        if let (Some(lt), Some(lp)) = (self.last_temp, self.last_pwm) {
            let pwm_delta = target_pwm.abs_diff(lp);
            let temp_delta = (temp - lt).abs();
            if pwm_delta < hp && temp_delta < ht {
                return lp; // hold; do not update anchors
            }
        }
        self.last_temp = Some(temp);
        self.last_pwm = Some(target_pwm);
        target_pwm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's real curve-1, stored unsorted exactly as in their config.
    fn owner_curve() -> SortedCurve {
        SortedCurve::new(vec![
            (29.0, 30.0),
            (52.0, 34.0),
            (69.0, 35.0),
            (89.0, 37.0),
            (40.0, 34.0),
            (78.0, 35.0),
        ])
    }

    #[test]
    fn owner_curve_live_anchor() {
        // Observed on real hardware: temp 41.3 °C → 34% → PWM 86.
        let c = owner_curve();
        let pct = c.eval(41.3); // between (40,34) and (52,34) → 34
        assert!((pct - 34.0).abs() < 0.001);
        assert_eq!(percent_to_pwm(pct), 86);
    }

    #[test]
    fn interpolation_boundaries() {
        let c = owner_curve();
        assert!((c.eval(10.0) - 30.0).abs() < 0.001); // below min → min speed
        assert!((c.eval(95.0) - 37.0).abs() < 0.001); // above max → max speed
        // midpoint of (29,30)-(40,34): temp 34.5 → 30 + 0.5*4 = 32
        assert!((c.eval(34.5) - 32.0).abs() < 0.001);
    }

    #[test]
    fn degenerate_curves() {
        assert!((SortedCurve::new(vec![]).eval(50.0) - 50.0).abs() < 0.001);
        assert!((SortedCurve::new(vec![(40.0, 25.0)]).eval(99.0) - 25.0).abs() < 0.001);
    }

    #[test]
    fn hysteresis_holds_then_releases() {
        let mut h = Hysteresis::default();
        assert_eq!(h.apply(40.0, 86, 1.0, 5), 86); // first: adopt
        assert_eq!(h.apply(40.4, 88, 1.0, 5), 86); // both deltas small: hold
        assert_eq!(h.apply(40.4, 92, 1.0, 5), 92); // pwm delta ≥ 5: release
        assert_eq!(h.apply(45.0, 93, 1.0, 5), 93); // temp delta ≥ 1.0: release
    }
}
```

- [ ] **Step 2: Register, test, commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 18 passed
git add -A && git commit -m "feat(daemon): curve interpolation + hysteresis (live-anchored tests)"
```

---

### Task 6: Fan slot resolution + keepalive policy (`fan.rs`)

**Files:**
- Create: `crates/llw-daemon/src/fan.rs`
- Modify: `crates/llw-daemon/src/main.rs` (add `mod fan;`)

- [ ] **Step 1: Write `fan.rs` (complete file)**

```rust
//! Per-tick fan decisions (pure): config slots → desired PWM bytes,
//! and the keepalive/send policy ported from upstream's fan_speed.rs
//! (policy lives here, not in llw-protocol — by design).

use crate::config::{DeviceConfig, SlotSpeed};
use crate::curve::percent_to_pwm;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Resolve a device's 4 slots to raw PWM bytes given each curve's current
/// speed %. Constraints (min duty etc.) are applied later by
/// `llw_protocol::frames::apply_pwm_constraints`.
pub fn resolve_slots(dev: &DeviceConfig, curve_pct: &HashMap<String, f32>) -> [u8; 4] {
    let mut pwm = [0u8; 4];
    for (i, slot) in dev.slots.iter().enumerate() {
        pwm[i] = match slot {
            SlotSpeed::Percent(pct) => percent_to_pwm(*pct as f32),
            SlotSpeed::Curve(name) => {
                percent_to_pwm(curve_pct.get(name).copied().unwrap_or(0.0))
            }
        };
    }
    pwm
}

/// Upstream send rule: transmit when any slot drifted (|desired − readback| > 5,
/// or desired ≤ 10 and readback differs at all), or when the keepalive interval
/// elapsed. Firmware reverts to hardware default without periodic traffic.
pub fn should_send(
    desired: &[u8; 4],
    readback: &[u8; 4],
    last_sent: Option<Instant>,
    now: Instant,
    keepalive: Duration,
) -> bool {
    let drifted = desired.iter().zip(readback.iter()).any(|(d, r)| {
        d.abs_diff(*r) > 5 || (*d <= 10 && *r != *d)
    });
    let keepalive_due = last_sent.map_or(true, |t| now.duration_since(t) >= keepalive);
    drifted || keepalive_due
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeviceConfig;

    fn dev(slots: [SlotSpeed; 4]) -> DeviceConfig {
        DeviceConfig { mac: "02:8b:51:62:32:e1".into(), name: None, slots, color: None }
    }

    #[test]
    fn resolves_curves_constants_and_off() {
        let d = dev([
            SlotSpeed::Curve("cpu".into()),
            SlotSpeed::Curve("cpu".into()),
            SlotSpeed::Percent(100),
            SlotSpeed::Percent(0),
        ]);
        let mut pct = HashMap::new();
        pct.insert("cpu".to_string(), 34.0);
        assert_eq!(resolve_slots(&d, &pct), [86, 86, 255, 0]);
    }

    #[test]
    fn unknown_curve_resolves_to_zero() {
        let d = dev([
            SlotSpeed::Curve("gone".into()),
            SlotSpeed::Percent(0),
            SlotSpeed::Percent(0),
            SlotSpeed::Percent(0),
        ]);
        assert_eq!(resolve_slots(&d, &HashMap::new()), [0, 0, 0, 0]);
    }

    #[test]
    fn send_policy() {
        let t0 = Instant::now();
        let ka = Duration::from_secs(1);
        // matched readback, keepalive not due → no send
        assert!(!should_send(&[86; 4], &[86; 4], Some(t0), t0 + Duration::from_millis(500), ka));
        // keepalive due → send even when matched
        assert!(should_send(&[86; 4], &[86; 4], Some(t0), t0 + ka, ka));
        // never sent → send
        assert!(should_send(&[86; 4], &[86; 4], None, t0, ka));
        // dropout signature [0,0,0,0] → drifted → send immediately
        assert!(should_send(&[86, 86, 86, 0], &[0, 0, 0, 0], Some(t0), t0, ka));
        // small drift within ±5 tolerated (readback jitter)
        assert!(!should_send(&[86; 4], &[84; 4], Some(t0), t0, ka));
        // low-PWM strictness: desired ≤ 10 must match exactly
        assert!(should_send(&[8, 0, 0, 0], &[10, 0, 0, 0], Some(t0), t0, ka));
    }
}
```

- [ ] **Step 2: Register, test, commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 21 passed
git add -A && git commit -m "feat(daemon): fan slot resolution + keepalive send policy"
```

---

### Task 7: Tiered-recovery state machine (`reliability.rs`)

The heart of M2 — spec §4.2 encoded as a pure state machine with injected timestamps, so every threshold, grace period, and cooldown is exhaustively testable. The owner's dropout timelines from `~/lianli-misbehave-*.log` informed the defaults (≥5 dropouts/60s after 120s grace).

**Files:**
- Create: `crates/llw-daemon/src/reliability.rs`
- Modify: `crates/llw-daemon/src/main.rs` (add `mod reliability;`)

- [ ] **Step 1: Write `reliability.rs` (complete file)**

```rust
//! Tiered link-recovery state machine (spec §4.2). Pure: all decisions are
//! functions of injected `Instant`s — no clocks, no I/O, no sleeps.
//!
//! Tier 1 (re-acquire): sustained PWM dropout → re-run channel acquisition.
//! Tier 2 (reconnect): repeated Tier-1 failure → full dongle reconnect.
//! The daemon supervisor (M2b) executes the returned `Action`s.

use crate::config::ReliabilityConfig;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Tier 1: reset + re-run scored acquisition + re-apply state.
    Reacquire,
    /// Tier 2: drop and reopen the dongle transports, full rediscovery.
    Reconnect,
}

pub struct Reliability {
    cfg: Cfg,
    dropouts: VecDeque<Instant>,
    acquired_at: Option<Instant>,
    last_tier1: Option<Instant>,
    last_tier2: Option<Instant>,
    failed_tier1_streak: u32,
    /// Telemetry counters (M2b exposes these over IPC).
    pub total_dropouts: u64,
    pub total_tier1: u64,
    pub total_tier2: u64,
}

/// Durations precomputed from the serializable config.
struct Cfg {
    grace: Duration,
    dropout_threshold: u32,
    window: Duration,
    tier1_cooldown: Duration,
    tier2_cooldown: Duration,
    tier2_after_failed_tier1: u32,
}

impl Reliability {
    pub fn new(cfg: &ReliabilityConfig) -> Self {
        Self {
            cfg: Cfg {
                grace: Duration::from_secs(cfg.grace_s),
                dropout_threshold: cfg.dropout_threshold.max(1),
                window: Duration::from_secs(cfg.window_s),
                tier1_cooldown: Duration::from_secs(cfg.tier1_cooldown_s),
                tier2_cooldown: Duration::from_secs(cfg.tier2_cooldown_s),
                tier2_after_failed_tier1: cfg.tier2_after_failed_tier1.max(1),
            },
            dropouts: VecDeque::new(),
            acquired_at: None,
            last_tier1: None,
            last_tier2: None,
            failed_tier1_streak: 0,
            total_dropouts: 0,
            total_tier1: 0,
            total_tier2: 0,
        }
    }

    /// Call after every successful acquisition (startup or recovery).
    /// Starts the grace period and clears transient state.
    pub fn on_acquired(&mut self, now: Instant) {
        self.acquired_at = Some(now);
        self.dropouts.clear();
    }

    /// Record one dropout observation (commanded PWM present, readback all-zero).
    pub fn on_dropout(&mut self, now: Instant) {
        self.total_dropouts += 1;
        self.dropouts.push_back(now);
        self.prune(now);
    }

    /// Decide what (if anything) to do right now.
    pub fn poll(&mut self, now: Instant) -> Action {
        self.prune(now);

        // Escalation: enough failed Tier-1 attempts → Tier 2, respecting cooldown.
        if self.failed_tier1_streak >= self.cfg.tier2_after_failed_tier1 {
            let cooled = self
                .last_tier2
                .map_or(true, |t| now.duration_since(t) >= self.cfg.tier2_cooldown);
            if cooled {
                self.last_tier2 = Some(now);
                self.total_tier2 += 1;
                self.failed_tier1_streak = 0;
                self.dropouts.clear();
                return Action::Reconnect;
            }
            return Action::None; // wait out the cooldown
        }

        // Tier 1: threshold within window, after grace, respecting cooldown.
        let in_grace = self
            .acquired_at
            .map_or(true, |t| now.duration_since(t) < self.cfg.grace);
        if in_grace {
            return Action::None;
        }
        if (self.dropouts.len() as u32) < self.cfg.dropout_threshold {
            return Action::None;
        }
        let cooled = self
            .last_tier1
            .map_or(true, |t| now.duration_since(t) >= self.cfg.tier1_cooldown);
        if !cooled {
            return Action::None;
        }

        self.last_tier1 = Some(now);
        self.total_tier1 += 1;
        self.dropouts.clear();
        Action::Reacquire
    }

    /// Report how the executed Tier-1 attempt went. Success resets the streak
    /// AND restarts the grace period (via on_acquired, called by the executor).
    pub fn on_tier1_result(&mut self, ok: bool) {
        if ok {
            self.failed_tier1_streak = 0;
        } else {
            self.failed_tier1_streak += 1;
        }
    }

    /// Dropouts currently inside the window (telemetry).
    pub fn recent_dropouts(&mut self, now: Instant) -> u32 {
        self.prune(now);
        self.dropouts.len() as u32
    }

    fn prune(&mut self, now: Instant) {
        while let Some(&front) = self.dropouts.front() {
            if now.duration_since(front) > self.cfg.window {
                self.dropouts.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReliabilityConfig;

    fn machine() -> Reliability {
        Reliability::new(&ReliabilityConfig::default())
    }

    /// t0 + seconds helper.
    fn ts(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    fn storm(r: &mut Reliability, t0: Instant, start_s: u64, n: u32) {
        for i in 0..n {
            r.on_dropout(ts(t0, start_s + i as u64));
        }
    }

    #[test]
    fn grace_period_suppresses_tier1() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 10, 10); // heavy storm right after acquisition
        assert_eq!(r.poll(ts(t0, 30)), Action::None); // still in 120s grace
    }

    #[test]
    fn dropout_storm_after_grace_fires_tier1_once() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 5); // ≥5 within 60s window, after grace
        assert_eq!(r.poll(ts(t0, 135)), Action::Reacquire);
        // immediately after: events cleared + cooldown → no refire
        assert_eq!(r.poll(ts(t0, 136)), Action::None);
        assert_eq!(r.total_tier1, 1);
    }

    #[test]
    fn below_threshold_never_fires() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 4); // one short of threshold
        assert_eq!(r.poll(ts(t0, 135)), Action::None);
    }

    #[test]
    fn window_pruning_forgets_old_dropouts() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 4);
        // 5th dropout arrives 70s later — the first 4 are outside the window
        r.on_dropout(ts(t0, 200));
        assert_eq!(r.poll(ts(t0, 201)), Action::None);
        assert_eq!(r.recent_dropouts(ts(t0, 201)), 1);
    }

    #[test]
    fn tier1_cooldown_gates_refire() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 5);
        assert_eq!(r.poll(ts(t0, 135)), Action::Reacquire);
        r.on_tier1_result(true);
        r.on_acquired(ts(t0, 136)); // recovery restarts grace
        // new storm right away: suppressed by fresh grace
        storm(&mut r, t0, 140, 5);
        assert_eq!(r.poll(ts(t0, 145)), Action::None);
        // after grace expires (136+120=256) AND cooldown passed → fires again
        storm(&mut r, t0, 260, 5);
        assert_eq!(r.poll(ts(t0, 265)), Action::Reacquire);
    }

    #[test]
    fn two_failed_tier1_escalate_to_tier2() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 5);
        assert_eq!(r.poll(ts(t0, 135)), Action::Reacquire);
        r.on_tier1_result(false); // acquisition failed — no on_acquired
        storm(&mut r, t0, 200, 5); // cooldown (60s) passed by t=195
        assert_eq!(r.poll(ts(t0, 205)), Action::Reacquire);
        r.on_tier1_result(false);
        // streak = 2 → escalate regardless of dropout state
        assert_eq!(r.poll(ts(t0, 206)), Action::Reconnect);
        assert_eq!(r.total_tier2, 1);
        // tier2 cooldown (300s) suppresses immediate repeat even if tier1 keeps failing
        r.on_tier1_result(false);
        r.on_tier1_result(false);
        assert_eq!(r.poll(ts(t0, 210)), Action::None);
        assert_eq!(r.poll(ts(t0, 520)), Action::Reconnect);
    }

    #[test]
    fn successful_tier1_resets_escalation_streak() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 5);
        assert_eq!(r.poll(ts(t0, 135)), Action::Reacquire);
        r.on_tier1_result(false);
        r.on_tier1_result(true); // second attempt succeeded
        assert_eq!(r.poll(ts(t0, 300)), Action::None); // no escalation
    }
}
```

- [ ] **Step 2: Register, test, commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 28 passed
git add -A && git commit -m "feat(daemon): tiered-recovery state machine (spec 4.2, pure + exhaustive tests)"
```

---

### Task 8: Static RGB assertion (`rgb_assert.rs`)

**Files:**
- Create: `crates/llw-daemon/src/rgb_assert.rs`
- Modify: `crates/llw-daemon/src/main.rs` (add `mod rgb_assert;`)

- [ ] **Step 1: Write `rgb_assert.rs` (complete file)**

```rust
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
```

- [ ] **Step 2: Register, test, commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 31 passed
git add -A && git commit -m "feat(daemon): static RGB assertion (brightness math + drift compare)"
```

---

### Task 9: `llw watch` diagnostic subcommand

The channel experiment's measurement tool: polls GetDev at an interval, prints per-device telemetry lines, counts dropouts, and can optionally command a PWM (with 1s keepalive) so dropout behavior is observable *while under control* — a mini fan-controller for experiments only.

**Files:**
- Modify: `crates/llw-cli/src/main.rs`

- [ ] **Step 1: Add the subcommand variant**

```rust
    /// Poll devices continuously, printing telemetry (Ctrl+C to stop)
    Watch {
        /// Poll interval in milliseconds
        #[arg(long, default_value_t = 500)]
        interval_ms: u64,
        /// Also command this PWM percent to ALL devices each second
        /// (makes dropouts observable: readback should track this value)
        #[arg(long)]
        pwm: Option<u8>,
    },
```

and the dispatch arm: `Command::Watch { interval_ms, pwm } => watch(interval_ms, pwm),`

- [ ] **Step 2: Add the implementation**

```rust
fn watch(interval_ms: u64, pwm: Option<u8>) -> Result<()> {
    if let Some(p) = pwm {
        if p > 100 {
            bail!("--pwm must be 0-100");
        }
    }
    let mut dongle = open_dongle()?;
    let master = dongle.discover_master().context("discovering master")?;
    println!(
        "Master {} on channel {} — watching every {}ms{} (Ctrl+C to stop)",
        mac_str(&master.mac),
        master.channel,
        interval_ms,
        pwm.map_or(String::new(), |p| format!(", commanding {p}% PWM")),
    );

    let mut dropouts: u64 = 0;
    let mut polls: u64 = 0;
    let mut last_pwm_send = std::time::Instant::now() - std::time::Duration::from_secs(2);
    let started = std::time::Instant::now();

    loop {
        // 1Hz keepalive when commanding (best-effort: skip this second if the
        // pre-send poll fails — the main poll below reports the error).
        if let Some(p) = pwm {
            if last_pwm_send.elapsed() >= std::time::Duration::from_secs(1) {
                if let Ok(report) = dongle.get_dev() {
                    for d in &report.devices {
                        let raw = (p as u16 * 255 / 100) as u8;
                        let mut target = [raw; 4];
                        llw_protocol::frames::apply_pwm_constraints(&mut target, d.kind, d.fan_count);
                        let rf = pwm_frame(&d.mac, &master.mac, d.rx_type, master.channel,
                                           d.list_index + 1, &target);
                        let _ = dongle.send_rf_frame(&rf, d.channel, d.rx_type);
                    }
                    last_pwm_send = std::time::Instant::now();
                }
            }
        }

        match dongle.get_dev() {
            Ok(report) => {
                polls += 1;
                for d in &report.devices {
                    let commanded = pwm.is_some();
                    let dropped = commanded
                        && d.fan_count > 0
                        && d.current_pwm.iter().take(d.fan_count as usize).all(|&x| x == 0);
                    if dropped {
                        dropouts += 1;
                    }
                    println!(
                        "{:>7.1}s [{}] ch={} rpm={:?} pwm={:?} fx={:02x?}{}  (polls={} dropouts={})",
                        started.elapsed().as_secs_f32(),
                        d.list_index,
                        d.channel,
                        d.fan_rpms,
                        d.current_pwm,
                        d.effect_index,
                        if dropped { "  << DROPOUT" } else { "" },
                        polls,
                        dropouts,
                    );
                }
            }
            Err(e) => println!(
                "{:>7.1}s poll error: {e}  (polls={polls} dropouts={dropouts})",
                started.elapsed().as_secs_f32()
            ),
        }
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    }
}
```

Import `llw_protocol::frames::pwm_frame` alongside the existing frames imports (it is already imported for `set_pwm` — reuse it).

- [ ] **Step 3: Build + help check (NO hardware run — production daemon owns the dongles)**

```bash
cargo build -p llw-cli 2>&1 | tail -2 && ./target/debug/llw watch --help
cargo clippy --workspace --all-targets 2>&1 | tail -3
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(cli): llw watch — continuous telemetry with optional PWM commanding"
```

---

### Task 10: Channel-behavior experiment (manual — with the owner, gates M2b)

**No files (findings recorded into this plan).** Requires the owner present; same dongle-takeover ritual as M1's Task 10. Budget ~20-30 minutes. The goal is to answer the questions M2b's acquisition design depends on:

- **Q1 — What determines the operating channel?** M1 showed GET_MAC answers on all channels, so scan responses don't. Candidates: CMD_RESET re-rolls it; or the channel byte used in command traffic steers it.
- **Q2 — Can we deliberately move the network to a chosen channel?** If yes, scored acquisition can pick a clean channel and steer. If no, Tier 1 becomes "reset-and-reroll until the dropout rate is acceptable" (still workable — it's what manual restarts effectively did).
- **Q3 — Does the dropout rate differ measurably by channel right now?** (The historical ch8-bad/ch2-good pattern, quantified with `llw watch --pwm`.)

- [ ] **Step 1: Take over the dongles**

```bash
systemctl --user stop lianli-watchdog.service lianli-daemon.service
lsusb | grep 0416   # both 8040 + 8041 present
```

- [ ] **Step 2: Baseline** — `./target/debug/llw devices` → record the current device channel.

- [ ] **Step 3: Reset-reroll distribution (Q1)** — repeat 6×:

```bash
./target/debug/llw reset && sleep 2 && ./target/debug/llw devices
```

Record the device-reported channel after each reset. If it changes across resets → CMD_RESET re-rolls the channel (Tier-1 "reroll" mechanism confirmed). If it never changes → channel is sticky; note what it sticks to.

- [ ] **Step 4: Steering probe (Q2)** — after a reset, immediately command PWM using a DIFFERENT channel byte than the devices report (this requires a one-off manual test: run `llw set-pwm 0 40 --hold` — which uses the *discovered* channel — while watching whether the device's reported channel converges to it). Then check `llw devices`: did the device's reported channel follow the commanded traffic's channel? Record yes/no. (If the CLI's discovered channel always equals the device channel, note that and record Q2 as "not testable without protocol modification — defer steering to M2b experiment task with a patched build".)

- [ ] **Step 5: Dropout rate under control (Q3)** — for each of 3 reset-rerolls, run 90 seconds of:

```bash
timeout 90 ./target/debug/llw watch --interval-ms 500 --pwm 40
```

Record per run: channel, polls, dropouts, and any `<< DROPOUT` bursts. This is the direct measurement of link quality per channel — the scoring signal for M2b.

- [ ] **Step 6: Restore production**

```bash
systemctl --user start lianli-daemon.service lianli-watchdog.service
```

- [ ] **Step 7: Record findings** — append a "## Channel-behavior experiment — results" section to THIS plan file: the reroll distribution table, the steering answer, the per-channel dropout table, and a one-paragraph "consequence for M2b acquisition design". Commit.

---

## Self-review notes (already applied)

- **Spec coverage (M2a slice):** §3.3 config (Task 2-3), §4.2 tiers (Task 7), §8 FakeTransport-style testing (Task 1), fan curves/hwmon (Tasks 4-6), static RGB re-assert building blocks (Task 8), §4.1 scored-acquisition groundwork (Tasks 9-10). Deliberately M2b: supervisor loop, acquisition implementation, IPC server, TX-wedge runtime detection, systemd/udev packaging, cutover + soak. Deliberately later: effects (M3), binding UI (M4), OpenRGB/LCD (post-v1).
- **Types:** `SlotSpeed`/`StaticColor`/`ReliabilityConfig` defined in Task 2, consumed in Tasks 3/6/7/8 with matching names. `FakeIo` (Task 1) is what M2b's simulation tests build on. `resolve_slots` output feeds `apply_pwm_constraints` (existing llw-protocol API) — order documented in fan.rs doc comment.
- **Known judgment calls:** `SlotSpeed` untagged serde matches upstream's config ergonomics (number or curve name); mb-sync import downgrades to 0 with a warning (mobo-sync support is a possible M2b/M3 addition — the GetDev mobo_pwm plumbing already exists in llw-protocol); `Ema` gates at 0–110 °C (upstream gates 0–100; widened slightly for hot NVMe/pump sensors, documented here); Task 9's watch subcommand contains one intentionally-flagged pseudo-call (`unwrap_or_default_report`) with explicit instructions to the implementer to write the real error-skip — flagged in-line so it cannot be pasted blindly.
