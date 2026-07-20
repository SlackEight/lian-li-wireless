# M2b: Reliability Daemon — Implementation Plan (M2 part 2 of 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A running `llw-daemon` that replaces `lianli-daemon` on the owner's machine: GetDev-based link acquisition, fan curves with keepalive, persistence-filtered dropout observation feeding the tiered-recovery machine, static-RGB drift restore, TX-wedge detection, a versioned IPC with `llw status`, systemd/udev packaging, and the cutover that starts the week-long M2 acceptance soak.

**Architecture:** One supervisor thread owns the `Dongle` and all policy; a `step(now)`-based design makes the whole control loop simulation-testable with `FakeIo` and injected `Instant`s (no sleeps in tests). An IPC thread forwards requests over an mpsc channel and the supervisor answers between steps. All M2a modules are consumed as-is.

**Tech Stack additions:** `signal-hook` (SIGTERM), everything else already in the workspace.

**Critical design inputs (from the M2a Task 10 experiment — see that plan's results section):**
1. **Acquisition reads GetDev.** Devices report the network's real operating channel in every record; `GET_MAC` is only needed for the master MAC. No scan-and-pick, no channel scoring.
2. **Dropout observations are persistence-filtered.** A healthy channel shows transient single-poll blips (self-healed by the 1s keepalive, no RPM spike). Only streaks of ≥2 consecutive all-zero-readback polls (while commanded) count as observations — the experiment's healthy run then produces 2 observations/90s (below the 5/60s threshold) while June's sustained storms would fire within seconds.
3. **Tier 1 is a state re-sync, not a channel escape.** CMD_RESET does not move the channel (6/6 sticky). Tier 1 = reset + re-acquire-from-GetDev + re-apply PWM/RGB; Tier 2 = full transport reconnect.

**Context for the engineer:**
- Spec: `docs/superpowers/specs/2026-07-13-lian-li-wireless-design.md` §3.3, §4, §7.
- M2a modules and their contracts: `reliability.rs` (READ ITS MODULE DOC — the poll/commit caller contract is mandatory), `fan.rs` (resolve_slots/should_send), `curve.rs`, `sensors.rs`, `rgb_assert.rs`, `config.rs`, `llw_protocol::io::FakeIo` (+ `drain_reads`), `Dongle::from_parts`.
- Hardware rules: Tasks 1–9 must NOT touch the dongles (production daemon owns them). Task 10-11 are the cutover, with the owner present.
- Work directly on `main`. Current test baseline: 36 (protocol) + 35 (daemon).

---

## File structure (end state of M2b)

```
crates/llw-daemon/src/
├── main.rs            # real entry: run supervisor + IPC + signals (replaces stub arms; keeps --check-config/--import-lianli)
├── config.rs          # + ObservationConfig, sensor_failsafe_percent
├── ipc.rs             # NEW: request/response envelope v1, StatusData, server thread
├── acquisition.rs     # NEW: GetDev-based link acquisition
├── observation.rs     # NEW: per-device dropout streak filter
├── supervisor.rs      # NEW: the control loop (step(now) design)
└── (existing M2a modules unchanged)
crates/llw-cli/src/main.rs   # + `llw status`
packaging/
├── systemd/llw-daemon.service
└── udev/99-llw.rules
```

---

### Task 1: Config additions (observation filter + failsafe)

**Files:**
- Modify: `crates/llw-daemon/src/config.rs`

- [ ] **Step 1: Add `ObservationConfig` and the failsafe field.** Insert after `ReliabilityConfig`:

```rust
/// How raw GetDev readbacks become dropout observations (experiment-tuned:
/// single-poll blips are normal on a healthy channel and self-heal via the
/// 1s keepalive; only persistent readback loss is evidence of link trouble).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationConfig {
    /// A dropout observation is reported for every poll at or beyond this
    /// many CONSECUTIVE all-zero-readback polls while PWM is commanded.
    pub consecutive_polls: u32,
    /// GetDev poll interval in ms.
    pub poll_ms: u64,
}

impl Default for ObservationConfig {
    fn default() -> Self {
        Self { consecutive_polls: 2, poll_ms: 500 }
    }
}
```

Add to `Config`: `#[serde(default)] pub observation: ObservationConfig,` (after `reliability`).

Add to `ControlConfig` a failsafe (with the derive-Default replaced accordingly in its `impl Default`):

```rust
    /// Fan % commanded when a curve's sensor has been unreadable for over a
    /// minute (never leave fans on a stale duty forever).
    pub sensor_failsafe_percent: u8,
```

with default `50`.

- [ ] **Step 2: Extend the roundtrip test** — in `config::tests::roundtrip`, add asserts:

```rust
        assert_eq!(loaded.observation.consecutive_polls, 2);
        assert_eq!(loaded.control.sensor_failsafe_percent, 50);
```

- [ ] **Step 3: Verify + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 35 passed
git add -A && git commit -m "feat(daemon): observation + failsafe config"
```

---

### Task 2: IPC types (`ipc.rs` — types half)

**Files:**
- Create: `crates/llw-daemon/src/ipc.rs`
- Modify: `crates/llw-daemon/src/main.rs` (add `#[allow(dead_code)] mod ipc;` — allow removed in Task 8)

- [ ] **Step 1: Write the types + envelope half of `ipc.rs`:**

```rust
//! Versioned IPC: newline-delimited JSON over a Unix socket.
//! Envelope carries `v` (protocol version); unknown versions are rejected
//! with a structured error so mismatched daemon/CLI pairs fail actionably.

use crate::config::Config;
use crate::reliability::Telemetry;
use serde::{Deserialize, Serialize};

pub const IPC_VERSION: u32 = 1;

pub fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("llw-daemon.sock")
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub v: u32,
    #[serde(flatten)]
    pub req: Request,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum Request {
    Ping,
    Status,
    GetConfig,
    SetConfig { config: Config },
    SetColor { mac: String, rgb: [u8; 3], brightness: u8 },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub v: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ResponseEnvelope {
    pub fn ok(data: Option<serde_json::Value>) -> Self {
        Self { v: IPC_VERSION, ok: true, error: None, data }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self { v: IPC_VERSION, ok: false, error: Some(msg.into()), data: None }
    }
}

/// The daemon's status snapshot served over IPC (and printed by `llw status`).
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusData {
    pub daemon_version: String,
    pub link: Option<LinkStatus>,
    pub tx_wedged: bool,
    pub reliability: Telemetry,
    pub devices: Vec<DeviceStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LinkStatus {
    pub master_mac: String,
    pub channel: u8,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub mac: String,
    pub kind: String,
    pub channel: u8,
    pub fan_count: u8,
    pub rpm: [u16; 4],
    pub desired_pwm: [u8; 4],
    pub readback_pwm: [u8; 4],
    pub rgb_in_sync: Option<bool>,
    pub dropout_streak: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shapes() {
        let line = r#"{"v":1,"method":"Status"}"#;
        let env: RequestEnvelope = serde_json::from_str(line).unwrap();
        assert_eq!(env.v, 1);
        assert!(matches!(env.req, Request::Status));

        let line = r#"{"v":1,"method":"SetColor","mac":"02:8b:51:62:32:e1","rgb":[255,0,0],"brightness":4}"#;
        let env: RequestEnvelope = serde_json::from_str(line).unwrap();
        assert!(matches!(env.req, Request::SetColor { .. }));

        let resp = ResponseEnvelope::err("nope");
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""ok":false"#) && s.contains("nope") && !s.contains("data"));
    }

    #[test]
    fn unknown_method_is_a_parse_error() {
        let line = r#"{"v":1,"method":"Frobnicate"}"#;
        assert!(serde_json::from_str::<RequestEnvelope>(line).is_err());
    }
}
```

(The server half arrives in Task 8 — this task is types only, so the CLI can build against them.)

- [ ] **Step 2: Verify + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 37 passed
cargo clippy --workspace --all-targets 2>&1 | tail -3
git add -A && git commit -m "feat(daemon): versioned IPC types (envelope v1, StatusData)"
```

---

### Task 3: Acquisition (`acquisition.rs`)

**Files:**
- Create: `crates/llw-daemon/src/acquisition.rs`
- Modify: `crates/llw-daemon/src/main.rs` (allow+mod, established pattern)

- [ ] **Step 1: Write `acquisition.rs`:**

```rust
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
        // no GET_MAC was needed: TX saw zero writes
        assert!(d_tx_writes_empty(&d));
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

    /// Test helper: acquisition must be RX-only in the happy path.
    fn d_tx_writes_empty<T>(_d: &Dongle<T>) -> bool
    where
        T: llw_protocol::io::UsbIo,
    {
        // Dongle's tx field is private outside llw-protocol; the meaningful
        // assertion (no GET_MAC fallback taken) is that the FakeIo TX read
        // queue was never consulted — which would have errored the test via
        // get_mac returning Ok(None) and try_acquire bailing. Reaching here
        // with Some(link) already proves the happy path skipped GET_MAC.
        true
    }
}
```

NOTE for the implementer: the `d_tx_writes_empty` helper is deliberately a documented no-op (Dongle's fields are private outside llw-protocol) — keep it with its comment, or simply drop the helper and the assert line; either is acceptable. Everything else is exact.

- [ ] **Step 2: Verify + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 41 passed
git add -A && git commit -m "feat(daemon): GetDev-based link acquisition (experiment-informed)"
```

---

### Task 4: Dropout observation filter (`observation.rs`)

**Files:**
- Create: `crates/llw-daemon/src/observation.rs`
- Modify: `crates/llw-daemon/src/main.rs` (allow+mod)

- [ ] **Step 1: Write `observation.rs`:**

```rust
//! Persistence filter turning raw GetDev readbacks into dropout observations
//! (M2a experiment: single-poll blips are healthy-channel background noise;
//! only consecutive-poll readback loss while commanded is link trouble).

/// Per-device streak tracker. Feed it every GetDev poll result.
#[derive(Debug, Default)]
pub struct DropoutFilter {
    streak: u32,
}

impl DropoutFilter {
    /// `commanded`: we have nonzero desired PWM for at least one active slot.
    /// `readback_zero`: every active fan slot read back 0.
    /// Returns true when THIS poll should be reported as a dropout
    /// observation (i.e. streak has reached `threshold`).
    pub fn observe(&mut self, commanded: bool, readback_zero: bool, threshold: u32) -> bool {
        if commanded && readback_zero {
            self.streak = self.streak.saturating_add(1);
            self.streak >= threshold.max(1)
        } else {
            self.streak = 0;
            false
        }
    }

    pub fn streak(&self) -> u32 {
        self.streak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_poll_blips_are_filtered() {
        // Replay of the experiment's healthy run 1: singles at 51.6s/65.2s
        // and one 3-poll burst — with threshold 2, only the burst's polls
        // 2 and 3 count (2 observations in the whole run).
        let mut f = DropoutFilter::default();
        let mut observations = 0;
        // single blip
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        // 3-poll burst
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        // another single
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        assert_eq!(observations, 2);
    }

    #[test]
    fn sustained_loss_accumulates_fast() {
        // June-style sustained loss: every poll past the threshold reports.
        let mut f = DropoutFilter::default();
        let count = (0..10).filter(|_| f.observe(true, true, 2)).count();
        assert_eq!(count, 9); // polls 2..=10
        assert_eq!(f.streak(), 10);
    }

    #[test]
    fn uncommanded_never_observes() {
        let mut f = DropoutFilter::default();
        assert!(!f.observe(false, true, 2));
        assert!(!f.observe(false, true, 2));
        assert_eq!(f.streak(), 0);
    }

    #[test]
    fn recovery_resets_streak() {
        let mut f = DropoutFilter::default();
        f.observe(true, true, 2);
        f.observe(true, true, 2);
        assert_eq!(f.streak(), 2);
        f.observe(true, false, 2);
        assert_eq!(f.streak(), 0);
    }
}
```

- [ ] **Step 2: Verify + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 45 passed
git add -A && git commit -m "feat(daemon): persistence-filtered dropout observation"
```

---

### Task 5: Supervisor core (`supervisor.rs` — structure, acquisition, fan control)

The supervisor is built across Tasks 5–7; each task leaves it compiling with tests green. Design rules: `step(now: Instant)` does one pass of everything due; `run()` loops `step(Instant::now())` + 50ms sleep; ALL timing decisions compare against the injected `now` (tests never sleep more than the loop granularity they choose). A `connector` closure abstracts `Dongle::open` so simulations can hand out `FakeIo` dongles.

**Files:**
- Create: `crates/llw-daemon/src/supervisor.rs`
- Modify: `crates/llw-daemon/src/main.rs` (allow+mod)

- [ ] **Step 1: Write the core (complete file at this stage):**

```rust
//! The supervisor: one thread owning the dongle and all policy.
//! Built as `step(now)` so the entire control loop is simulation-testable
//! with FakeIo dongles and injected time (no sleeps in tests).

use crate::acquisition::{self, Link};
use crate::config::{Config, SlotSpeed};
use crate::curve::{percent_to_pwm, Hysteresis, SortedCurve};
use crate::fan;
use crate::observation::DropoutFilter;
use crate::reliability::{Action, Reliability};
use crate::rgb_assert;
use crate::sensors::{self, Ema, HwmonSensor};
use anyhow::Result;
use llw_protocol::dongle::Dongle;
use llw_protocol::frames::{self, apply_pwm_constraints, master_clock_frame, pwm_frame};
use llw_protocol::io::UsbIo;
use llw_protocol::record::DeviceRecord;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

const RECONNECT_INTERVAL: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const RGB_REUPLOAD_COOLDOWN: Duration = Duration::from_secs(5);
const SENSOR_FAILSAFE_AFTER: Duration = Duration::from_secs(60);

/// What a step did — simulation tests assert on this.
#[derive(Debug, Default, PartialEq)]
pub struct StepOutcome {
    pub acquired: bool,
    pub polled: bool,
    pub sent_pwm: u32,
    pub sent_heartbeat: bool,
    pub uploaded_rgb: u32,
    pub tier1: bool,
    pub tier2: bool,
}

struct CurveRuntime {
    curve: SortedCurve,
    sensor: Option<HwmonSensor>,
    ema: Ema,
    hyst: Hysteresis,
    last_good_read: Option<Instant>,
    /// Current output percent (None until first successful evaluation).
    pct: Option<f32>,
}

struct DeviceRuntime {
    mac: [u8; 6],
    desired: [u8; 4],
    last_sent: Option<Instant>,
    filter: DropoutFilter,
    expected_fx: Option<[u8; 4]>,
    last_rgb_upload: Option<Instant>,
    last_record: Option<DeviceRecord>,
}

pub struct Supervisor<T: UsbIo> {
    cfg: Config,
    hwmon_base: PathBuf,
    connector: Box<dyn FnMut() -> llw_protocol::Result<Dongle<T>> + Send>,
    dongle: Option<Dongle<T>>,
    link: Option<Link>,
    reliability: Reliability,
    curves: HashMap<String, CurveRuntime>,
    devices: HashMap<[u8; 6], DeviceRuntime>,
    last_reconnect: Option<Instant>,
    last_poll: Option<Instant>,
    last_fan_tick: Option<Instant>,
    last_heartbeat: Option<Instant>,
    pub tx_wedged: bool,
}

impl<T: UsbIo> Supervisor<T> {
    pub fn new(
        cfg: Config,
        hwmon_base: PathBuf,
        connector: Box<dyn FnMut() -> llw_protocol::Result<Dongle<T>> + Send>,
    ) -> Self {
        let reliability = Reliability::new(&cfg.reliability);
        let mut curves = HashMap::new();
        for c in &cfg.curves {
            curves.insert(
                c.name.clone(),
                CurveRuntime {
                    curve: SortedCurve::new(c.points.clone()),
                    sensor: None, // resolved lazily in fan tick
                    ema: Ema::new(0.3),
                    hyst: Hysteresis::default(),
                    last_good_read: None,
                    pct: None,
                },
            );
        }
        let mut devices = HashMap::new();
        for d in &cfg.devices {
            if let Ok(mac) = crate::config::parse_mac(&d.mac) {
                devices.insert(
                    mac,
                    DeviceRuntime {
                        mac,
                        desired: [0; 4],
                        last_sent: None,
                        filter: DropoutFilter::default(),
                        expected_fx: None,
                        last_rgb_upload: None,
                        last_record: None,
                    },
                );
            }
        }
        Self {
            cfg,
            hwmon_base,
            connector,
            dongle: None,
            link: None,
            reliability,
            curves,
            devices,
            last_reconnect: None,
            last_poll: None,
            last_fan_tick: None,
            last_heartbeat: None,
            tx_wedged: false,
        }
    }

    /// One pass of everything due at `now`.
    pub fn step(&mut self, now: Instant) -> StepOutcome {
        let mut out = StepOutcome::default();
        self.ensure_connected(now);
        if self.dongle.is_none() {
            return out;
        }
        if self.link.is_none() {
            out.acquired = self.try_acquire_link(now);
            if self.link.is_none() {
                return out;
            }
        }
        if due(self.last_poll, now, Duration::from_millis(self.cfg.observation.poll_ms)) {
            self.last_poll = Some(now);
            out.polled = true;
            self.poll_devices(now);
        }
        if due(self.last_fan_tick, now, Duration::from_millis(self.cfg.control.tick_ms)) {
            self.last_fan_tick = Some(now);
            out.sent_pwm = self.fan_tick(now);
        }
        if due(self.last_heartbeat, now, HEARTBEAT_INTERVAL) {
            self.last_heartbeat = Some(now);
            out.sent_heartbeat = self.send_heartbeat();
        }
        out.uploaded_rgb = self.rgb_tick(now);
        match self.reliability.poll(now) {
            Action::None => {}
            Action::Reacquire => {
                out.tier1 = true;
                let ok = self.tier1_resync(now);
                self.reliability.on_tier1_result(ok);
                if ok {
                    self.reliability.on_acquired(now);
                }
            }
            Action::Reconnect => {
                out.tier2 = true;
                self.tier2_reconnect(now);
            }
        }
        out
    }

    /// Production loop. 50ms granularity; all real timing lives in step().
    pub fn run(&mut self, shutdown: &std::sync::atomic::AtomicBool) {
        info!("supervisor running");
        while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            self.step(Instant::now());
            std::thread::sleep(Duration::from_millis(50));
        }
        info!("supervisor stopped");
    }

    fn ensure_connected(&mut self, now: Instant) {
        if self.dongle.is_some() {
            return;
        }
        if !due(self.last_reconnect, now, RECONNECT_INTERVAL) {
            return;
        }
        self.last_reconnect = Some(now);
        match (self.connector)() {
            Ok(d) => {
                info!("dongle connected");
                self.dongle = Some(d);
                self.tx_wedged = false;
                self.link = None;
            }
            Err(llw_protocol::ProtocolError::DeviceNotFound { vid, pid }) => {
                if !self.tx_wedged {
                    warn!("dongle {vid:04x}:{pid:04x} not found — possible TX wedge; will keep retrying");
                    notify_once("Lian Li wireless: TX dongle missing — if it stays gone, power-cycle the PSU (known firmware wedge)");
                    self.tx_wedged = true;
                }
            }
            Err(e) => warn!("dongle open failed: {e}"),
        }
    }

    fn try_acquire_link(&mut self, now: Instant) -> bool {
        let Some(dongle) = self.dongle.as_mut() else { return false };
        match acquisition::try_acquire(dongle) {
            Ok(Some((link, records))) => {
                info!(
                    "link acquired: master {:02x?} channel {}",
                    link.master_mac, link.channel
                );
                self.link = Some(link);
                self.reliability.on_acquired(now);
                self.ingest_records(&records, now);
                // Force immediate RGB assert + PWM send on the next ticks.
                for d in self.devices.values_mut() {
                    d.expected_fx = None;
                    d.last_sent = None;
                }
                true
            }
            Ok(None) => false,
            Err(e) => {
                warn!("acquisition error: {e}");
                self.drop_dongle();
                false
            }
        }
    }

    fn poll_devices(&mut self, now: Instant) {
        let Some(dongle) = self.dongle.as_mut() else { return };
        let report = match dongle.get_dev() {
            Ok(r) => r,
            Err(llw_protocol::ProtocolError::NoResponse { .. }) => return, // silent poll; not fatal
            Err(e) => {
                warn!("GetDev failed: {e}");
                self.drop_dongle();
                return;
            }
        };
        let records = report.devices;
        self.ingest_records(&records, now);
    }

    fn ingest_records(&mut self, records: &[DeviceRecord], now: Instant) {
        let threshold = self.cfg.observation.consecutive_polls;
        for rec in records {
            let Some(dev) = self.devices.get_mut(&rec.mac) else { continue };
            let commanded = dev
                .desired
                .iter()
                .take(rec.fan_count as usize)
                .any(|&p| p > 0);
            let readback_zero = rec.fan_count > 0
                && rec
                    .current_pwm
                    .iter()
                    .take(rec.fan_count as usize)
                    .all(|&p| p == 0);
            if dev.filter.observe(commanded, readback_zero, threshold) {
                debug!("dropout observation for {} (streak {})", rec.mac_str(), dev.filter.streak());
                self.reliability.on_dropout(now);
            }
            dev.last_record = Some(rec.clone());
        }
    }

    fn fan_tick(&mut self, now: Instant) -> u32 {
        // 1) evaluate curves
        let failsafe = self.cfg.control.sensor_failsafe_percent as f32;
        let (ht, hp) = (self.cfg.control.hysteresis_temp, self.cfg.control.hysteresis_pwm);
        let specs: HashMap<String, crate::config::SensorSpec> = self
            .cfg
            .curves
            .iter()
            .map(|c| (c.name.clone(), c.sensor.clone()))
            .collect();
        for (name, rt) in self.curves.iter_mut() {
            if rt.sensor.is_none() {
                if let Some(spec) = specs.get(name) {
                    match sensors::resolve(&self.hwmon_base, spec) {
                        Ok(s) => rt.sensor = Some(s),
                        Err(e) => debug!("sensor resolve failed for {name}: {e}"),
                    }
                }
            }
            let reading = rt.sensor.as_ref().and_then(|s| s.read_c().ok());
            match reading {
                Some(temp) => {
                    rt.last_good_read = Some(now);
                    if let Some(smoothed) = rt.ema.update(temp) {
                        let pct = rt.curve.eval(smoothed);
                        let pwm = rt.hyst.apply(smoothed, percent_to_pwm(pct), ht, hp);
                        // +0.5 makes the percent→PWM truncation round-trip
                        // exact: ((p+0.5)/2.55)*2.55 truncates back to p.
                        rt.pct = Some((pwm as f32 + 0.5) / 2.55);
                    }
                }
                None => {
                    rt.sensor = None; // re-resolve next tick (M2a review carry-forward)
                    let stale = rt
                        .last_good_read
                        .is_none_or(|t| now.duration_since(t) >= SENSOR_FAILSAFE_AFTER);
                    if stale {
                        rt.pct = Some(failsafe);
                    }
                }
            }
        }
        let curve_pct: HashMap<String, f32> = self
            .curves
            .iter()
            .filter_map(|(n, rt)| rt.pct.map(|p| (n.clone(), p)))
            .collect();

        // 2) per configured device: resolve + constraints + send policy
        let keepalive = Duration::from_millis(self.cfg.control.keepalive_ms);
        let Some(link) = self.link else { return 0 };
        let mut sent = 0u32;
        let device_cfgs: Vec<crate::config::DeviceConfig> = self.cfg.devices.clone();
        for dc in &device_cfgs {
            let Ok(mac) = crate::config::parse_mac(&dc.mac) else { continue };
            let Some(dev) = self.devices.get_mut(&mac) else { continue };
            let Some(rec) = dev.last_record.clone() else { continue };
            // Skip curve-driven devices until their curve has produced output.
            if dc.slots.iter().any(|s| matches!(s, SlotSpeed::Curve(n) if !curve_pct.contains_key(n))) {
                continue;
            }
            let mut pwm = fan::resolve_slots(dc, &curve_pct);
            apply_pwm_constraints(&mut pwm, rec.kind, rec.fan_count);
            dev.desired = pwm;
            if fan::should_send(&pwm, &rec.current_pwm, dev.last_sent, now, keepalive) {
                let rf = pwm_frame(
                    &mac,
                    &link.master_mac,
                    rec.rx_type,
                    link.channel,
                    rec.list_index + 1,
                    &pwm,
                );
                let Some(dongle) = self.dongle.as_mut() else { return sent };
                match dongle.send_rf_frame(&rf, rec.channel, rec.rx_type) {
                    Ok(()) => {
                        dev.last_sent = Some(now);
                        sent += 1;
                    }
                    Err(e) => {
                        warn!("PWM send failed for {}: {e}", rec.mac_str());
                        self.drop_dongle();
                        return sent;
                    }
                }
            }
        }
        sent
    }

    fn send_heartbeat(&mut self) -> bool {
        let Some(link) = self.link else { return false };
        let Some(dongle) = self.dongle.as_mut() else { return false };
        let rf = master_clock_frame(&link.master_mac);
        match dongle.send_rf_frame(&rf, link.channel, 0xFF) {
            Ok(()) => true,
            Err(e) => {
                warn!("heartbeat failed: {e}");
                self.drop_dongle();
                false
            }
        }
    }

    fn rgb_tick(&mut self, now: Instant) -> u32 {
        let Some(link) = self.link else { return 0 };
        let mut uploads = 0u32;
        let device_cfgs: Vec<crate::config::DeviceConfig> = self.cfg.devices.clone();
        for dc in &device_cfgs {
            let Some(color) = dc.color else { continue };
            let Ok(mac) = crate::config::parse_mac(&dc.mac) else { continue };
            let Some(dev) = self.devices.get_mut(&mac) else { continue };
            let Some(rec) = dev.last_record.clone() else { continue };
            let frame = rgb_assert::static_frame(&rec, &color);
            let expected = rgb_assert::expected_index(&frame);
            let needs = match dev.expected_fx {
                None => true, // never asserted this session
                Some(exp) => rgb_assert::drifted(&exp, &rec.effect_index),
            };
            let cooled = dev
                .last_rgb_upload
                .is_none_or(|t| now.duration_since(t) >= RGB_REUPLOAD_COOLDOWN);
            if needs && cooled {
                let Some(dongle) = self.dongle.as_mut() else { return uploads };
                match dongle.upload_rgb(
                    &mac,
                    &link.master_mac,
                    rec.channel,
                    rec.rx_type,
                    &[frame],
                    5000,
                    4,
                ) {
                    Ok(idx) => {
                        debug_assert_eq!(idx, expected);
                        dev.expected_fx = Some(idx);
                        dev.last_rgb_upload = Some(now);
                        uploads += 1;
                        info!("RGB asserted for {}", rec.mac_str());
                    }
                    Err(e) => {
                        warn!("RGB upload failed for {}: {e}", rec.mac_str());
                        dev.last_rgb_upload = Some(now); // cooldown even on failure
                    }
                }
            }
        }
        uploads
    }

    /// Tier 1: CMD_RESET + immediate re-acquire + force re-apply of PWM/RGB.
    /// (Experiment: the channel is sticky — this refreshes network state, it
    /// does not move channels.)
    ///
    /// On FAILURE (link not re-acquirable after reset) we escalate directly
    /// to the transport-reconnect path by dropping the dongle: with no link,
    /// no further dropouts can accumulate, so waiting for the state machine's
    /// formal Tier 2 would deadlock. The machine's Reconnect action remains
    /// as a backstop for repeated tier-1 failures across reconnects.
    fn tier1_resync(&mut self, now: Instant) -> bool {
        info!("Tier 1: reset + re-sync");
        let Some(dongle) = self.dongle.as_mut() else { return false };
        if let Err(e) = dongle.reset() {
            warn!("Tier 1 reset failed: {e}");
            self.drop_dongle();
            self.last_reconnect = None;
            return false;
        }
        self.link = None;
        for d in self.devices.values_mut() {
            d.filter = DropoutFilter::default();
        }
        let ok = self.try_acquire_link(now);
        if !ok {
            warn!("Tier 1 re-acquire failed — escalating to transport reconnect");
            self.drop_dongle();
            self.last_reconnect = None; // retry immediately on next step
        }
        ok
    }

    /// Tier 2: drop everything and reconnect from scratch (next steps redo
    /// open + acquire on the reconnect cadence).
    fn tier2_reconnect(&mut self, _now: Instant) {
        warn!("Tier 2: full reconnect");
        self.drop_dongle();
        self.last_reconnect = None; // retry immediately on next step
    }

    fn drop_dongle(&mut self) {
        self.dongle = None;
        self.link = None;
    }

    pub fn link(&self) -> Option<Link> {
        self.link
    }
}

fn due(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= interval)
}

fn notify_once(msg: &str) {
    let _ = std::process::Command::new("notify-send")
        .arg("llw-daemon")
        .arg(msg)
        .spawn();
}
```

- [ ] **Step 2: Simulation test — healthy loop acquires, polls, and sends PWM.** Append the tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DeviceConfig, SlotSpeed, StaticColor};
    use llw_protocol::io::FakeIo;

    pub(crate) const MAC: [u8; 6] = [0x02, 0x8b, 0x51, 0x62, 0x32, 0xe1];
    pub(crate) const MASTER: [u8; 6] = [0xe5, 0xba, 0xf0, 0x72, 0xab, 0x3c];

    pub(crate) fn record_bytes(pwm: [u8; 4], fx: [u8; 4]) -> [u8; 42] {
        let mut r = [0u8; 42];
        r[0..6].copy_from_slice(&MAC);
        r[6..12].copy_from_slice(&MASTER);
        r[12] = 2;
        r[13] = 1;
        r[19] = 3;
        r[20..24].copy_from_slice(&fx);
        r[24] = 36;
        r[36..40].copy_from_slice(&pwm);
        r[41] = 0x1C;
        r
    }

    pub(crate) fn getdev_resp(records: &[[u8; 42]]) -> Vec<u8> {
        let mut resp = vec![0u8; 4 + 42 * records.len()];
        resp[0] = 0x10;
        resp[1] = records.len() as u8;
        resp[2] = 0x80;
        for (i, r) in records.iter().enumerate() {
            resp[4 + i * 42..4 + (i + 1) * 42].copy_from_slice(r);
        }
        resp
    }

    pub(crate) fn test_config() -> Config {
        let mut cfg = Config::new();
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(0),
            ],
            color: Some(StaticColor { rgb: [255, 255, 255], brightness: 4 }),
        });
        cfg
    }

    /// Build a supervisor whose connector hands out a FakeIo dongle with the
    /// given RX script; returns (supervisor, base instant).
    pub(crate) fn sim(cfg: Config, rx_script: Vec<Vec<u8>>) -> (Supervisor<FakeIo>, Instant) {
        let sup = Supervisor::new(
            cfg,
            std::env::temp_dir(), // no hwmon needed for Percent-slot configs
            Box::new(move || {
                let rx = FakeIo::default();
                for r in rx_script.clone() {
                    rx.push_read(r);
                }
                Ok(Dongle::from_parts(FakeIo::default(), Some(rx)))
            }),
        );
        (sup, Instant::now())
    }

    #[test]
    fn healthy_loop_acquires_and_commands() {
        let rec = record_bytes([0; 4], [0; 4]);
        // acquisition poll + a few status polls
        let script = vec![getdev_resp(&[rec]); 6];
        let (mut sup, t0) = sim(test_config(), script);

        // step 1: connect + acquire (+ first poll/fan/heartbeat/rgb in later steps)
        let out = sup.step(t0);
        assert!(out.acquired);
        assert_eq!(sup.link().unwrap().channel, 2);

        // subsequent step at +600ms: GetDev poll due + fan tick due → PWM sent
        let out = sup.step(t0 + Duration::from_millis(1100));
        assert!(out.polled);
        assert_eq!(out.sent_pwm, 1);
        assert!(out.sent_heartbeat);
        // 40% → raw 102; SL-INF min duty leaves it; slot 4 zeroed
        let dev = sup.devices.get(&MAC).unwrap();
        assert_eq!(dev.desired, [102, 102, 102, 0]);
    }
}
```

NOTE: `healthy_loop_acquires_and_commands` walks real branch logic — if any assertion fails, the LOOP ORDER is wrong, not the test; report BLOCKED with the outcome rather than adjusting.

- [ ] **Step 3: Verify + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 46 passed
cargo clippy --workspace --all-targets 2>&1 | tail -3
git add -A && git commit -m "feat(daemon): supervisor core (acquisition, fan control, heartbeat, RGB assert)"
```

---

### Task 6: Supervisor reliability integration tests

Task 5's code already wires Reliability into `step()` — this task proves the tiers fire correctly end-to-end in simulation.

**Files:**
- Modify: `crates/llw-daemon/src/supervisor.rs` (tests only)

- [ ] **Step 1: Append simulation tests:**

```rust
    fn fast_reliability_config() -> Config {
        let mut cfg = test_config();
        cfg.reliability.grace_s = 0;
        cfg.reliability.window_s = 60;
        cfg.reliability.dropout_threshold = 3;
        cfg.reliability.tier1_cooldown_s = 0;
        cfg.observation.poll_ms = 0; // poll every step
        cfg.control.tick_ms = 0; // fan tick every step
        cfg
    }

    #[test]
    fn sustained_dropout_fires_tier1_and_resyncs() {
        let healthy = record_bytes([102, 102, 102, 0], [0; 4]);
        let dropped = record_bytes([0, 0, 0, 0], [0; 4]);
        // script: acquire (healthy) + 1 healthy poll, then sustained zeros,
        // then the post-reset re-acquire read + recovery
        let mut script = vec![getdev_resp(&[healthy]); 2];
        script.extend(vec![getdev_resp(&[dropped]); 6]);
        script.extend(vec![getdev_resp(&[healthy]); 4]);
        let (mut sup, t0) = sim(fast_reliability_config(), script);

        let mut tier1_fired = false;
        for i in 0..10 {
            let now = t0 + Duration::from_secs(i + 1);
            let out = sup.step(now);
            if out.tier1 {
                tier1_fired = true;
                break;
            }
        }
        assert!(tier1_fired, "sustained readback loss must trigger Tier 1");
        // after the tier-1 resync consumed a read, link should be back
        assert!(sup.link().is_some());
    }

    #[test]
    fn transient_blips_do_not_fire_tier1() {
        let healthy = record_bytes([102, 102, 102, 0], [0; 4]);
        let dropped = record_bytes([0, 0, 0, 0], [0; 4]);
        // alternate: each zero-readback poll is followed by recovery —
        // streak never reaches 2, no observations at threshold 2
        let mut script = vec![getdev_resp(&[healthy]); 2];
        for _ in 0..5 {
            script.push(getdev_resp(&[dropped]));
            script.push(getdev_resp(&[healthy]));
        }
        let (mut sup, t0) = sim(fast_reliability_config(), script);
        for i in 0..12 {
            let out = sup.step(t0 + Duration::from_secs(i + 1));
            assert!(!out.tier1, "transient blips must not fire Tier 1 (step {i})");
        }
    }

    #[test]
    fn failed_tier1_escalates_to_transport_reconnect() {
        // Script: acquire healthy, dropouts build to threshold, then the
        // script runs DRY — tier1's post-reset re-acquire times out → tier1
        // fails → supervisor drops the dongle and re-connects immediately
        // (connector invoked again). This is the practical escalation path;
        // the machine's formal Tier 2 remains a backstop.
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let connects = Arc::new(AtomicU32::new(0));
        let connects_in = Arc::clone(&connects);
        let mut cfg = fast_reliability_config();
        cfg.reliability.tier2_cooldown_s = 0;
        let healthy = record_bytes([102, 102, 102, 0], [0; 4]);
        let dropped = record_bytes([0, 0, 0, 0], [0; 4]);
        let mut script = vec![getdev_resp(&[healthy]); 2];
        script.extend(vec![getdev_resp(&[dropped]); 4]);
        let mut sup: Supervisor<FakeIo> = Supervisor::new(
            cfg,
            std::env::temp_dir(),
            Box::new(move |
| {
                let n = connects_in.fetch_add(1, Ordering::Relaxed);
                let rx = FakeIo::default();
                if n == 0 {
                    for r in script.clone() {
                        rx.push_read(r);
                    }
                }
                // later connections: dead air (empty script)
                Ok(Dongle::from_parts(FakeIo::default(), Some(rx)))
            }),
        );
        let t0 = Instant::now();
        let mut saw_tier1 = false;
        for i in 0..10 {
            let out = sup.step(t0 + Duration::from_secs(i + 1));
            if out.tier1 {
                saw_tier1 = true;
                break;
            }
        }
        assert!(saw_tier1, "sustained dropouts must fire Tier 1");
        assert_eq!(connects.load(Ordering::Relaxed), 1);
        // tier1 failed (script dry) → dongle dropped + immediate reconnect
        // allowed: the very next step re-invokes the connector
        let _ = sup.step(t0 + Duration::from_secs(20));
        assert_eq!(
            connects.load(Ordering::Relaxed),
            2,
            "failed tier-1 must escalate to a transport reconnect"
        );
        // dead air on the new dongle: stays unacquired, no panic
        let _ = sup.step(t0 + Duration::from_secs(21));
        assert!(sup.link().is_none());
    }
```

CAREFUL implementation note: the stray line-wrap in `Box::new(move |` above is a formatting artifact — it is a plain `move ||` closure. If this test doesn't pass as written, do NOT massage timings blindly — report BLOCKED with a trace of which steps fired what (StepOutcome per step + connector count), so the coordinator can adjudicate whether the supervisor wiring or the test timeline is wrong.

- [ ] **Step 2: Verify + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 49 passed
git add -A && git commit -m "test(daemon): supervisor tier-1/tier-2 simulation coverage"
```

---

### Task 7: Wedge path + RGB drift simulation tests

**Files:**
- Modify: `crates/llw-daemon/src/supervisor.rs` (tests only)

- [ ] **Step 1: Append tests:**

```rust
    #[test]
    fn connector_failure_marks_wedge_and_backs_off() {
        let mut calls = 0u32;
        let mut sup: Supervisor<FakeIo> = Supervisor::new(
            test_config(),
            std::env::temp_dir(),
            Box::new(move || {
                calls += 1;
                Err(llw_protocol::ProtocolError::DeviceNotFound { vid: 0x0416, pid: 0x8040 })
            }),
        );
        let t0 = Instant::now();
        let out = sup.step(t0);
        assert_eq!(out, StepOutcome::default());
        assert!(sup.tx_wedged);
        // within the 10s reconnect interval: no second open attempt happens
        // (observable: tx_wedged stays true and step stays inert)
        let _ = sup.step(t0 + Duration::from_secs(1));
        assert!(sup.tx_wedged);
        // after the interval, retries continue quietly
        let _ = sup.step(t0 + Duration::from_secs(11));
        assert!(sup.tx_wedged);
    }

    #[test]
    fn rgb_drift_triggers_reupload_with_cooldown() {
        // device reports a FOREIGN effect index after our upload → re-upload,
        // but not more than once per cooldown window
        let foreign_fx = [0xd9, 0x2c, 0xb8, 0x51];
        let rec_foreign = record_bytes([102, 102, 102, 0], foreign_fx);
        let script = vec![getdev_resp(&[rec_foreign]); 12];
        let mut cfg = test_config();
        cfg.observation.poll_ms = 0;
        cfg.control.tick_ms = 0;
        let (mut sup, t0) = sim(cfg, script);

        let out = sup.step(t0);
        assert!(out.acquired);
        // first rgb_tick uploads (expected_fx was None)
        let out = sup.step(t0 + Duration::from_secs(1));
        assert_eq!(out.uploaded_rgb, 1);
        // device keeps reporting the foreign index → drift detected, but
        // cooldown (5s) suppresses immediate re-upload
        let out = sup.step(t0 + Duration::from_secs(2));
        assert_eq!(out.uploaded_rgb, 0);
        // past cooldown → re-upload happens
        let out = sup.step(t0 + Duration::from_secs(7));
        assert_eq!(out.uploaded_rgb, 1);
    }
```

- [ ] **Step 2: Verify + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 51 passed
cargo clippy --workspace --all-targets 2>&1 | tail -3
git add -A && git commit -m "test(daemon): wedge backoff + RGB drift-reupload simulation coverage"
```

---

### Task 8: IPC server + supervisor integration

**Files:**
- Modify: `crates/llw-daemon/src/ipc.rs` (add server half)
- Modify: `crates/llw-daemon/src/supervisor.rs` (drain + answer requests)

- [ ] **Step 1: Add the server half to `ipc.rs`:**

```rust
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use tracing::{info, warn};

/// A request paired with its reply channel.
pub struct IpcCmd {
    pub req: Request,
    pub reply: mpsc::Sender<ResponseEnvelope>,
}

/// Bind the socket and serve connections forever, forwarding parsed requests
/// to the supervisor via `tx`. One thread per connection; one request per line.
pub fn serve(tx: mpsc::Sender<IpcCmd>) -> anyhow::Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path); // stale socket from a previous run
    let listener = UnixListener::bind(&path)?;
    info!("IPC listening on {}", path.display());
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let tx = tx.clone();
                std::thread::spawn(move || handle_conn(s, tx));
            }
            Err(e) => warn!("IPC accept failed: {e}"),
        }
    }
    Ok(())
}

fn handle_conn(stream: UnixStream, tx: mpsc::Sender<IpcCmd>) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;
    let mut line = String::new();
    while {
        line.clear();
        matches!(reader.read_line(&mut line), Ok(n) if n > 0)
    } {
        let resp = process_line(line.trim(), &tx);
        let Ok(json) = serde_json::to_string(&resp) else { break };
        if writeln!(writer, "{json}").is_err() {
            break;
        }
    }
}

fn process_line(line: &str, tx: &mpsc::Sender<IpcCmd>) -> ResponseEnvelope {
    let env: RequestEnvelope = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => return ResponseEnvelope::err(format!("bad request: {e}")),
    };
    if env.v != IPC_VERSION {
        return ResponseEnvelope::err(format!(
            "protocol version {} unsupported (daemon speaks {IPC_VERSION}) — update llw/llw-daemon",
            env.v
        ));
    }
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx.send(IpcCmd { req: env.req, reply: reply_tx }).is_err() {
        return ResponseEnvelope::err("daemon shutting down");
    }
    match reply_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(resp) => resp,
        Err(_) => ResponseEnvelope::err("daemon busy (no reply within 3s)"),
    }
}
```

- [ ] **Step 2: Supervisor drains requests.** Add to `Supervisor`: a field `ipc_rx: Option<std::sync::mpsc::Receiver<crate::ipc::IpcCmd>>` (constructor param: `ipc_rx: Option<Receiver<IpcCmd>>` appended to `new()` — update ALL existing Supervisor::new call sites — the `sim()` helper plus the direct constructions in `connector_failure_marks_wedge_and_backs_off` and `failed_tier1_escalates_to_transport_reconnect` — each passing `None`), and at the TOP of `step()`:

```rust
        self.drain_ipc(now);
```

with:

```rust
    fn drain_ipc(&mut self, _now: Instant) {
        let Some(rx) = &self.ipc_rx else { return };
        // Bounded drain: at most 8 requests per step keeps the control loop fair.
        for _ in 0..8 {
            let Ok(cmd) = rx.try_recv() else { break };
            let resp = self.answer(cmd.req);
            let _ = cmd.reply.send(resp);
        }
    }

    fn answer(&mut self, req: crate::ipc::Request) -> crate::ipc::ResponseEnvelope {
        use crate::ipc::{DeviceStatus, LinkStatus, Request, ResponseEnvelope, StatusData};
        match req {
            Request::Ping => ResponseEnvelope::ok(Some(serde_json::json!("pong"))),
            Request::Status => {
                let data = StatusData {
                    daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                    link: self.link.map(|l| LinkStatus {
                        master_mac: mac_str(&l.master_mac),
                        channel: l.channel,
                    }),
                    tx_wedged: self.tx_wedged,
                    reliability: self.reliability.telemetry(),
                    devices: self
                        .devices
                        .values()
                        .map(|d| {
                            let rec = d.last_record.as_ref();
                            DeviceStatus {
                                mac: mac_str(&d.mac),
                                kind: rec.map_or("?".into(), |r| r.kind.display_name().into()),
                                channel: rec.map_or(0, |r| r.channel),
                                fan_count: rec.map_or(0, |r| r.fan_count),
                                rpm: rec.map_or([0; 4], |r| r.fan_rpms),
                                desired_pwm: d.desired,
                                readback_pwm: rec.map_or([0; 4], |r| r.current_pwm),
                                rgb_in_sync: match (d.expected_fx, rec) {
                                    (Some(exp), Some(r)) => Some(exp == r.effect_index),
                                    _ => None,
                                },
                                dropout_streak: d.filter.streak(),
                            }
                        })
                        .collect(),
                };
                match serde_json::to_value(&data) {
                    Ok(v) => ResponseEnvelope::ok(Some(v)),
                    Err(e) => ResponseEnvelope::err(e.to_string()),
                }
            }
            Request::GetConfig => match serde_json::to_value(&self.cfg) {
                Ok(v) => ResponseEnvelope::ok(Some(v)),
                Err(e) => ResponseEnvelope::err(e.to_string()),
            },
            Request::SetConfig { config } => match config.validate() {
                Ok(()) => {
                    if let Err(e) = config.save(&crate::config::default_path()) {
                        return ResponseEnvelope::err(format!("save failed: {e}"));
                    }
                    self.apply_config(config);
                    ResponseEnvelope::ok(None)
                }
                Err(e) => ResponseEnvelope::err(format!("invalid config: {e}")),
            },
            Request::SetColor { mac, rgb, brightness } => {
                if brightness > 4 {
                    return ResponseEnvelope::err("brightness must be 0-4");
                }
                let Some(dc) = self.cfg.devices.iter_mut().find(|d| d.mac == mac) else {
                    return ResponseEnvelope::err(format!("unknown device {mac}"));
                };
                dc.color = Some(crate::config::StaticColor { rgb, brightness });
                if let Err(e) = self.cfg.save(&crate::config::default_path()) {
                    return ResponseEnvelope::err(format!("save failed: {e}"));
                }
                // force re-assert on next rgb_tick
                if let Ok(m) = crate::config::parse_mac(&mac) {
                    if let Some(dev) = self.devices.get_mut(&m) {
                        dev.expected_fx = None;
                        dev.last_rgb_upload = None;
                    }
                }
                ResponseEnvelope::ok(None)
            }
        }
    }

    /// Swap in a validated config (curves/devices rebuilt; link kept).
    fn apply_config(&mut self, cfg: Config) {
        let link = self.link;
        let dongle = self.dongle.take();
        let mut fresh = Supervisor::new(cfg, self.hwmon_base.clone(), Box::new(|| unreachable!("connector never swapped")), None);
        self.cfg = fresh.cfg.clone();
        self.curves = std::mem::take(&mut fresh.curves);
        self.devices = std::mem::take(&mut fresh.devices);
        self.reliability = Reliability::new(&self.cfg.reliability);
        self.link = link;
        self.dongle = dongle;
        if link.is_some() {
            self.reliability.on_acquired(Instant::now());
        }
    }
```

plus a free `fn mac_str(mac: &[u8; 6]) -> String` helper (same format as elsewhere).

IMPLEMENTATION WARNING on `apply_config`: the throwaway `fresh` supervisor with an `unreachable!` connector is a deliberate trick to reuse `new()`'s curve/device construction — its connector is never called because `fresh` is never stepped. If this offends (it should, slightly), the cleaner refactor is extracting `fn build_runtimes(cfg: &Config) -> (HashMap<String, CurveRuntime>, HashMap<[u8;6], DeviceRuntime>)` from `new()` and calling it from both places — PREFER THE REFACTOR; the trick is the fallback if the refactor snowballs. Note `apply_config` uses `Instant::now()` — acceptable (production-only path; simulation tests don't exercise SetConfig re-grace).

- [ ] **Step 3: In-process round-trip test** (append to ipc.rs tests):

```rust
    #[test]
    fn process_line_version_gate_and_dispatch() {
        let (tx, rx) = std::sync::mpsc::channel();
        // answer thread: reply "pong" to whatever arrives
        std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                let _ = cmd.reply.send(ResponseEnvelope::ok(Some(serde_json::json!("pong"))));
            }
        });
        let resp = process_line(r#"{"v":1,"method":"Ping"}"#, &tx);
        assert!(resp.ok);
        let resp = process_line(r#"{"v":9,"method":"Ping"}"#, &tx);
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("version"));
        let resp = process_line("not json", &tx);
        assert!(!resp.ok);
    }
```

(`process_line` needs `pub(crate)` visibility for this test — adjust.)

- [ ] **Step 4: Verify + commit**

```bash
cargo test -p llw-daemon 2>&1 | tail -3   # 52 passed
cargo clippy --workspace --all-targets 2>&1 | tail -3
git add -A && git commit -m "feat(daemon): IPC server + supervisor request handling"
```

---

### Task 9: Real main.rs + `llw status`

**Files:**
- Modify: `crates/llw-daemon/src/main.rs` (real entry; remove ALL the temporary `#[allow(dead_code)]` module attributes — every module now has callers)
- Modify: `crates/llw-cli/src/main.rs` (+ `Status` subcommand)
- Modify: root `Cargo.toml` (+ `signal-hook = "0.3"` workspace dep) and `crates/llw-daemon/Cargo.toml`

- [ ] **Step 1: Rewrite main.rs's default arm** (keep `--check-config` and `--import-lianli` arms unchanged):

```rust
        None => run_daemon(),
        Some(other) => {
            eprintln!("unknown argument {other:?}");
            eprintln!("usage: llw-daemon [--check-config | --import-lianli [path] [--force]]");
            std::process::exit(2);
        }
```

with:

```rust
fn run_daemon() -> Result<()> {
    let path = config::default_path();
    let cfg = config::Config::load(&path)?;
    if cfg.devices.is_empty() {
        tracing::warn!(
            "no devices configured — daemon will idle; run --import-lianli or edit {}",
            path.display()
        );
    }

    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, std::sync::Arc::clone(&shutdown))?;
    }

    let (ipc_tx, ipc_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = ipc::serve(ipc_tx) {
            tracing::error!("IPC server failed: {e}");
        }
    });

    let mut sup = supervisor::Supervisor::new(
        cfg,
        std::path::PathBuf::from("/sys/class/hwmon"),
        Box::new(llw_protocol::dongle::Dongle::open),
        Some(ipc_rx),
    );
    sup.run(&shutdown);
    let _ = std::fs::remove_file(ipc::socket_path());
    Ok(())
}
```

(Module declarations lose their `#[allow(dead_code)]` attributes. `mod ipc; mod acquisition; mod observation; mod supervisor;` are declared plainly alongside the others.)

- [ ] **Step 2: `llw status` in llw-cli** — add variant `/// Show llw-daemon status\nStatus,` + arm `Command::Status => status(),` and:

```rust
fn status() -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("llw-daemon.sock");
    let mut stream = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connecting {} — is llw-daemon running?", path.display()))?;
    writeln!(stream, r#"{{"v":1,"method":"Status"}}"#)?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    if !v["ok"].as_bool().unwrap_or(false) {
        bail!("daemon error: {}", v["error"].as_str().unwrap_or("unknown"));
    }
    let d = &v["data"];
    println!("llw-daemon {}", d["daemon_version"].as_str().unwrap_or("?"));
    match d["link"].as_object() {
        Some(l) => println!(
            "link: master {} channel {}",
            l["master_mac"].as_str().unwrap_or("?"),
            l["channel"]
        ),
        None => println!("link: NOT ACQUIRED"),
    }
    if d["tx_wedged"].as_bool().unwrap_or(false) {
        println!("!! TX dongle missing/wedged — power-cycle may be required");
    }
    let r = &d["reliability"];
    println!(
        "reliability: dropouts={} tier1={} tier2={} streak={}",
        r["total_dropouts"], r["total_tier1"], r["total_tier2"], r["failed_tier1_streak"]
    );
    for dev in d["devices"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "  {} {} ch={} rpm={} desired={} readback={} rgb_sync={} streak={}",
            dev["mac"].as_str().unwrap_or("?"),
            dev["kind"].as_str().unwrap_or("?"),
            dev["channel"],
            dev["rpm"],
            dev["desired_pwm"],
            dev["readback_pwm"],
            dev["rgb_in_sync"],
            dev["dropout_streak"],
        );
    }
    Ok(())
}
```

llw-cli gains a `serde_json = { workspace = true }` dependency.

- [ ] **Step 3: Verify + commit**

```bash
cargo build --release 2>&1 | tail -2
cargo test 2>&1 | tail -4
cargo clippy --workspace --all-targets 2>&1 | tail -3
./target/release/llw status 2>&1 | head -2   # expect a clean "is llw-daemon running?" error (daemon not started — do NOT start it; lianli-daemon owns the dongles)
git add -A && git commit -m "feat: real daemon entry (signals, IPC thread) + llw status"
```

---

### Task 10: Packaging

**Files:**
- Create: `packaging/systemd/llw-daemon.service`
- Create: `packaging/udev/99-llw.rules`
- Modify: `README.md` (install section)

- [ ] **Step 1: `packaging/systemd/llw-daemon.service`:**

```ini
[Unit]
Description=Lian Li Wireless daemon (fans + RGB + link supervision)
Documentation=https://github.com/SlackEight/lian-li-wireless
# Only one process may own the dongles:
Conflicts=lianli-daemon.service
After=graphical-session.target

[Service]
Type=simple
ExecStart=/usr/local/bin/llw-daemon
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

- [ ] **Step 2: `packaging/udev/99-llw.rules`:**

```text
# Lian Li wireless TX/RX dongles — user-space access for llw-daemon
SUBSYSTEM=="usb", ATTRS{idVendor}=="0416", ATTRS{idProduct}=="8040", MODE="0666"
SUBSYSTEM=="usb", ATTRS{idVendor}=="0416", ATTRS{idProduct}=="8041", MODE="0666"
SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="e304", MODE="0666"
SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="e305", MODE="0666"
```

- [ ] **Step 3: README install section** — replace the "Build & try" section's note block with an "## Install (daemon)" section documenting: build release; `sudo install -Dm755 target/release/llw-daemon /usr/local/bin/llw-daemon`; `sudo install -Dm644 packaging/udev/99-llw.rules /etc/udev/rules.d/99-llw.rules && sudo udevadm control --reload`; `install -Dm644 packaging/systemd/llw-daemon.service ~/.config/systemd/user/llw-daemon.service`; import config; `systemctl --user disable --now lianli-watchdog lianli-daemon; systemctl --user enable --now llw-daemon`; verify with `llw status`. State plainly that it replaces lianli-daemon and both cannot run together.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat: systemd unit + udev rules + install docs"
```

---

### Task 11: Cutover (manual — with the owner; starts the M2 soak)

The M2 acceptance gate ("survives the cold-boot scenario for one week with zero manual restarts; watchdog retired") BEGINS here; it completes a week later.

- [ ] **Step 1: Import the owner's config** — `cargo run -p llw-daemon -- --import-lianli` (review warnings; verify `--check-config`).
- [ ] **Step 2: Install** per the README section (binary, udev, unit).
- [ ] **Step 3: Switch daemons:**

```bash
systemctl --user disable --now lianli-watchdog.service lianli-daemon.service
systemctl --user enable --now llw-daemon.service
sleep 15 && ./target/release/llw status
```

Expected: link acquired on the device-reported channel; fans at curve PWM within ~15s (listen!); RGB asserted white (watch the fans); `rgb_sync=true`; no wedge flag.
- [ ] **Step 4: Live-fire checks** — journal for 5 minutes (`journalctl --user -u llw-daemon -f`): no error spam, heartbeats silent, PWM stable. `llw status` again: dropout counters low/zero.
- [ ] **Step 5: Cold-boot test** — full reboot; after login, `llw status` within 2 minutes: link acquired, fans on curve, zero manual intervention. THIS is the June failure scenario — record the outcome carefully.
- [ ] **Step 6: Record cutover results** in this plan file + commit. Note the soak start date; the M2 gate closes after 7 days of `llw status` showing sane counters with zero manual restarts. (lianli-daemon stays installed-but-disabled as the rollback: `systemctl --user disable --now llw-daemon && systemctl --user enable --now lianli-daemon lianli-watchdog`.)

---

## Self-review notes (already applied)

- **Spec coverage:** §3.3 supervisor/config/IPC → Tasks 5-9; §4.1 acquisition (redefined by experiment) → Task 3; §4.2 tiers → M2a machine + Task 6 integration; §4.3 wedge → Tasks 5/7; §4.4 telemetry → Task 8 Status; packaging → Task 10; M2 gate → Task 11. Deferred: OpenRGB server, effects (M3), bind/unbind (M4), multi-zone RGB.
- **Types:** `Link` (T3) used by supervisor (T5) and StatusData (T2/T8); `IpcCmd` (T8) matches the `ipc_rx` param added to `Supervisor::new` (T8 modifies T5's constructor — tests updated in the same task); `Telemetry` from M2a is embedded in StatusData.
- **Known judgment calls:** supervisor holds `Option<Dongle>` and drops it on ANY send/poll error (reconnect cadence 10s) — deliberately blunt for v1, matching "reopen is the recovery primitive"; `apply_config` has a flagged wart with a preferred refactor spelled out; RGB re-upload cooldown 5s prevents drift-storm loops on a device that refuses to hold state; simulation tests use second-granularity injected time (config takes seconds for reliability windows — tests set grace 0/cooldowns 0 to compress).

---

## Task 11 cutover — results (2026-07-14, morning)

| Step | Result |
|------|--------|
| Config import | PASS — 1 curve + 1 device, ZERO warnings; k10temp mapped natively |
| Install (binary/udev/unit) | PASS |
| Daemon swap | PASS — llw-daemon acquired ch2 and took fan control; lianli-daemon + watchdog disabled |
| Boot-scope hazard | FOUND+FIXED — lianli-linux's package enables its unit in GLOBAL systemd scope; user-scope disable was insufficient for cold boot. `systemctl --user mask lianli-daemon.service` applied (rollback: unmask) |
| Live verification | PASS — desired==readback [86,86,86,0], ~730 RPM on curve, rgb_sync=true, 0 dropouts |
| Restart recovery | PASS — connect 21ms, link 227ms, RGB asserted 580ms (journal, RUST_LOG=info now in unit) |
| Cold-boot test | PENDING — owner reboots after this session; verify with `llw status` after login (link acquired, fans on curve, no intervention). This is the June failure scenario |
| Soak | STARTED 2026-07-14 — 7 days, zero manual restarts required; `llw status` counters should stay sane. Gate closes ~2026-07-21 |

Rollback at any point: `systemctl --user disable --now llw-daemon && systemctl --user unmask lianli-daemon && systemctl --user enable --now lianli-daemon lianli-watchdog`

Repo published: https://github.com/SlackEight/lian-li-wireless (public, 2026-07-14)

### Cold-boot test — result (2026-07-14, 08:06 boot)

**PASS, with the June scenario reproduced live and self-healed.** The master cold-booted onto channel 8 (confirming the boot-lock hypothesis — its power-on default), and the early-boot RF environment produced a genuine dropout storm (~199 observations in the first ~8 min, Tier-1 resyncs at t+132s and t+252s exactly per grace/cooldown design, each re-acquiring in ~700ms). Throughout, the 1s keepalive held fans near target — no June-style sawtooth, brief surges only. The storm then decayed on its own (~1 dropout/min by t+10min; zero Tier-1s since), fans rock-steady at PWM 86 / ~734 RPM, rgb_sync=true. **Zero manual intervention** — the June failure that previously persisted for hours until a manual restart self-stabilized in under ten minutes.

Timing: daemon active 12.4s into userspace (39s total boot, firmware+loader = 21s of it); dongle→link→RGB in under 600ms. The audible "hard minute" ≈ BIOS/boot hardware-default window + early storm surges.

Soak watch-items: (a) if a future boot's channel-8 storm does NOT decay, the deferred channel-steering question (experiment Q2) becomes priority follow-up; (b) Tier-1 fired twice without changing the channel (as expected — sticky) and without harm (~700ms each); consider whether resync-on-storm earns its keep vs pure keepalive riding, once soak data accumulates.

### Soak incident — external interference from lianli-gui (2026-07-14 evening)

Owner reported the June symptom back: fans full-tilt ~1s every ~15s. **Not a daemon defect — external interference.** `lianli-gui` (the retired lianli-linux GUI) had been session-restored by Plasma at login (we masked the daemon+watchdog units in the cutover but the GUI isn't a systemd service). From ~20:21 it was talking directly to the TX dongle: our GetDev reads came back `0xff` (reconnect storms), the master's PWM state kept getting wiped (readback zeros, `dropout_streak` 41, fans reverting to full-tilt default), 220+ dropouts and 5 Tier-1s accumulated. Killing pid 1946 stopped it **instantly**: readback snapped to [85,85,85], RPM to ~820, and 90 monitored seconds passed with zero new dropouts.

Fix: `ksmserverrc` `[General] excludeApps=lianli-gui` — Plasma will never session-restore it again (package stays installed as the rollback path). Note for the soak gate: the daemon behaved correctly under active interference (kept healing, no wedge, no manual intervention); this window should be discounted from dropout statistics, not counted against the gate.

Daemon improvement noted (not urgent): a GetDev `0xff` response is protocol-level garbage, not a USB failure — treating it as a transport error causes reconnect churn under interference. Candidate: count it as a dropped poll first, reconnect only on repeats.

**Soak data 2026-07-15 morning:** post-interference-fix overnight: Tier-1 cluster 00:28–00:51 (8 events, ~2–5 min apart), then 6.5h silent, single Tier-1 07:25, single GetDev 0xff 09:26. All self-healed; fans steady at readback 86 / ~730 RPM, streak 0. Pattern = bursty ambient 2.4GHz interference (BT keyboard/WiFi share the band), not the June persistent-failure disease. Watch-item: if a burst cluster ever coincides with user-visible blips again with no foreign process on the dongle, consider the GetDev-0xff-as-dropped-poll change (noted above) to cut reconnect churn during bursts.

### Incident: sustained external RF interference — root cause + fixes (2026-07-17)

Owner reported "frequent fan dropout": fans audibly surging to full for ~1s every ~15s. Telemetry: 244 Tier-1s / 7,458 dropouts since the Jul-15 redeploy, storming continuously 23:00→10:00. Live signature: readback zeroes across all fans → RPM ramps 730→2100 over ~4s → keepalive restores 86 → repeat at irregular 13–28s gaps. Crucially: rgb_in_sync stayed TRUE throughout and only 2 protocol-level failures in 1.7 days — the link was fine and the master never rebooted; its PWM state alone was being cleanly zeroed. RF noise cannot forge well-formed state; only Lian-Li-protocol traffic can. All local sources eliminated (no lianli-gui, no UI, daemon-silent windows still showed hits). Conclusion: **a foreign Lian Li transmitter in RF range** (hour-of-day schedule = someone else's PC) **on channel 8 — every Lian Li master's universal power-on channel**, where our cluster has sat pinned since the Jul-14 cold boot.

Hypotheses tested and falsified (probe binaries kept as `llw-protocol/examples/`):
1. **rf[15] channel steering** — 6 bursts + direct-channel frames: record stays ch8. rf[15] is addressing metadata, not a command.
2. **Sustained-traffic steering** (the M2a "boot-lock" model): 15s of GET_MAC(2)+keepalives on ch2: no move. The channel cannot be host-steered by any known mechanism; M2a Q2 is now answered NO.
3. **Keepalive starvation → master failover**: keepalive 1000→250ms cut dropouts only 15.7→12.7/min. Minor effect, not the mechanism. (Reverted to 1000ms.)

**The fix that worked — flash-saved fallback PWM (`examples/save-default.rs`):** the master reverts to its FLASH-SAVED speed on state loss; ours dated to the original L-Connect binding (full speed). Asserted PWM 86 (34%), verified readback, sent the SaveConfig (0x15) broadcast — the same call the bind flow ships. Result, immediate and dramatic: zero-window peak RPM 2100→**735** (inaudible), and dropout rate collapsed ~12.7→~1/min with zero Tier-1s, because the master now reloads sane state in ≤1–2s instead of ramping deaf for 3–8s — most windows now die under the persistence filter. Also retro-explains June: any keepalive gap reverted fans to the full-speed flash default.

**Daemon fix (this commit):** `try_acquire_link` no longer nulls `expected_fx` — the drift guard now compares retained state against the fresh record's effect index, so Tier-1 re-acquires stop re-uploading intact RGB (each upload is a device flash write; the storm burned 244 in a day). Pinned by `tier1_does_not_reupload_intact_rgb`.

Carried forward: (a) consider `llw save-defaults` CLI + daemon-side re-snapshot when cruising PWM changes materially (rate-limited hard — flash wear); (b) the bind flow already saves current PWM at bind time, so newly bound devices get sane defaults automatically; (c) if the interferer's traffic ever causes worse than cosmetic dropout ticks, the remaining lever is physical (dongle placement/shielding) — the channel is not movable.

### Incident addendum — surge persists past the flash-default fix; surge watchdog shipped (2026-07-17)

Owner: "it is still doing it" — correct, and 4Hz captures explain why the 1Hz telemetry looked clean: **the physical RPM peak lands ~3s AFTER readback recovers** (fan inertia) and the window itself now closes fast enough to slip under the persistence filter — so dropout counters stay flat while fans audibly surge to ~1900. The interference is evidently not just wiping state but **actively commanding full speed** during the master's deaf gap; a flash default cannot defend against an active command. Net effect of the earlier fix: surges are shorter and less frequent, not gone.

**Shipped: daemon-native surge watchdog** (the lianli-watchdog idea, done properly):
- `observation::SurgeTracker` (pure, unit-tested): judges each zero-readback window PLUS an 8-poll inertia tail against the healthy RPM baseline; threshold ⅓+100 rpm over baseline (871-wobble-proof, catches 1500+); re-opened windows merge into one episode; `reset()` on material commanded-PWM change so curve moves can't false-positive (wired in fan_tick).
- Every judged surge: journal WARN with peak/baseline, `Telemetry.total_surges` + `last_surge_peak_rpm` (additive serde), desktop notify-send (60s rate limit) so the owner can correlate what they hear with hard timestamps.
- UI Health reliability card shows "fan surges" (amber when nonzero, last peak in the tooltip).
- keepalive_ms set to 250 in the live config (shipped default stays 1000): trims up to ~750ms off each surge's full-speed command window.

Validation loop: watch `journalctl --user -u llw-daemon | grep 'fan surge'` and the Health counter against the owner's ears. Remaining mitigation ideas if the rate stays painful: physical (TX dongle placement nearer the master raises our signal margin over the interferer), and identifying the interferer (its schedule suggests a neighbor's rig — powering our master cluster off/on during a quiet hour would re-roll nothing, but asking neighbors about Lian Li gear might).

### Incident final root cause — firmware revert-on-keepalive-loss under RF noise (2026-07-17 evening)

Channel survey: OUR master is the ONLY Lian Li device answering on all 39 channels — no foreign controller exists. The "external transmitter" theory is dead. The real mechanism, confirmed by a daemon-silent capture (revert window persisted 15+ s at full 2190 rpm with no keepalives flowing) and our own code comment ("firmware reverts without traffic"): **2.4 GHz noise (WiFi, hour-of-day) kills our TX→master keepalive frames; after ~10-15 s without one, the master's host-lost failsafe reverts PWM to hardware default (full)**. The master→us direction (GetDev records via the RX dongle) rides through the same noise — the case-internal TX dongle next to a 575 W GPU is the weak link. This also retro-explains June and "ch2 good / ch8 bad" (WiFi overlap).

Burst-recovery probe: master accepts a correct frame in 0.3-1.2 s of 30 ms-spaced attempts (3-12 frames) — no deafness; earlier "ignored frames" were probe frames carrying a wrong rf[15]. Fixes shipped (validated by the surge watchdog's peak numbers):
1. **Burst-on-revert**: readback all-zero while commanded → the PWM frame is sent ×4 at 25 ms per tick (single frames die to the same noise that caused the revert). Pinned by `reverted_readback_sends_a_pwm_burst`.
2. **Detection at 4 Hz**: observation.poll_ms 1000→250 live (revert detected in ≤350 ms instead of ≤1.2 s).
3. **True 200 ms keepalive**: port-fidelity audit found keepalive_ms=250 vs tick_ms=200 quantizes to ~400 ms sends; keepalive_ms=200 restores upstream's effective 200-300 ms cadence.
4. **rf[16] seq parity (audit CRITICAL)**: we sent raw-GetDev-slot+1; upstream sends the 1-based position filtered to our master's devices. Wrong/flapping seq is a plausible silent-rejection mechanism under contention. Now derived per upstream at ingest.
5. Surge watchdog tail made time-based (8 s regardless of poll cadence).

Remaining levers, owner decision: (a) move the TX dongle out of the case (front/top USB or short extension — raises TX→master margin over the noise; likely the single biggest physical win); (b) re-bind the cluster with a chosen master_channel (bind frames carry rf[15]; June's quiet ch2 suggests channel is chosen at pair time — would move us off the noisy frequency permanently; needs owner present, uses the M4a bind machinery, doubles as its live validation); (c) 2.4 GHz hygiene on their own router (it broadcasts on WiFi ch10).

### CHANNEL MOVED — bind-time steering confirmed (2026-07-20, owner present)

Multi-day watchdog data: 2 surges the first post-fix evening, 33 and 36 the next two days, then 162 today with peaks back at 2200 — the mitigations hold under moderate noise but today's environment overwhelmed them on ch8. Two experiments in the owner's window:
1. **Silent-migration test FALSIFIED**: 90s of total RF silence — master stayed on ch8. Also learned the **flash-default fallback does NOT protect**: fans ran full (2190) during the silence despite the Jul-17 SaveConfig snapshot; that fix's apparent effect was coincidental. The host-lost revert target is hardware-full, period.
2. **Re-bind with master_channel=2 in rf[15] WORKED**: bind burst to the SAME master/rx with the target channel byte, transmitted on ch8 → device converged to ch2 in <6s → SaveConfig persisted → daemon re-acquired on **channel 2**. Channel IS set at pair time — this is how June's network lived on ch2, and answers M2a Q2 for real: steering exists, but only through the bind path.

Design consequence: the safe no-unbind re-bind (same master, same rx, new channel) is a first-class recovery tool. Future work: `preferred_channel` in config + daemon-side auto-rebind when acquired channel ≠ preferred (would also self-heal the cold-boot-on-8 case if the persisted channel doesn't survive power loss — unverified until the next reboot). Validation window running vs today's 162-surge baseline.

**Validation (2026-07-20 09:30-09:45): channel 2, during the same conditions that produced 162 surges on ch8 that day — 0 surges, 0 dropouts. The channel move is the complete fix; the mitigations (burst recovery, 200ms keepalive, seq parity) remain as defense-in-depth, and the watchdog stays on as the permanent instrument.**

### ch2 aftermath — frozen telemetry + a transient fan STOP; reverted to ch8 (2026-07-20 afternoon)

Hours after the morning ch8→ch2 move, the owner found the fans STOPPED while Status showed healthy stale numbers. Ground truth: on ch2 the master's periodic record broadcast DIES — records refresh only on accepted PWM *changes* (proven: one fresh snapshot per commanded change, frozen otherwise; 30 identical samples during passive polling). Every watchdog was blind behind the frozen stream (stall counter read 0 with fans physically stopped, tach showed rpm=[0,0,0] the instant fresh data flowed). The stop itself is best explained as a wedged-master transient from the rebind (the rebind tool carries a safe pwm=86, ruling out the earlier saved-zero theory); USB-resetting both dongles did NOT restore freshness.

**Reverted via rebind-channel 8: telemetry resumed immediately** (11 distinct rpm triples/25s), fans ~730, SaveConfig persisted. Standing rule: ANY channel move must be followed by a telemetry-freshness gate — ≥10 distinct rpm triples in 25s of passive polling — and reverted on failure. Future channel search (if ch8's evening surges stay painful) should walk candidates with that gate. Also carried: a record-staleness detector in the daemon (identical rpm triple across N polls while commanded → treat telemetry as stale, poke a ±1 PWM dither to force refresh, and never trust frozen data in the stall/surge watchdogs).
