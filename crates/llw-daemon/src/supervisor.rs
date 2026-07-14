//! The supervisor: one thread owning the dongle and all policy.
//! Built as `step(now)` so the entire control loop is simulation-testable
//! with FakeIo dongles and injected time (no sleeps in tests).

use crate::acquisition::{self, Link};
use crate::config::{Config, SlotSpeed};
use crate::curve::{percent_to_pwm, Hysteresis, SortedCurve};
use crate::effects_bridge;
use crate::fan;
use crate::observation::DropoutFilter;
use crate::reliability::{Action, Reliability};
use crate::rgb_assert;
use crate::sensors::{self, Ema, HwmonSensor};
use llw_protocol::dongle::Dongle;
use llw_protocol::frames::{apply_pwm_constraints, master_clock_frame, pwm_frame};
use llw_protocol::io::UsbIo;
use llw_protocol::record::DeviceRecord;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

const RECONNECT_INTERVAL: Duration = Duration::from_secs(10);
const AIR_EXPIRY: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const RGB_REUPLOAD_COOLDOWN: Duration = Duration::from_secs(5);
/// After a successful RGB upload the supervisor holds RF traffic (GetDev polls,
/// PWM keepalives, heartbeat) for this long so the firmware can commit the
/// flash-write without interference.
///
/// Diagnosis: the standalone probe (which stays silent for ~3s after upload)
/// always sticks; the daemon (which resumes 500ms polls and 1s keepalives
/// immediately) never sticks — the RF traffic during the firmware's flash-commit
/// window aborts the store, producing the observed 5s drift-retry loop.
///
/// Keepalive-gap trade-off: a 3s PWM gap may cause a transient fan-speed revert
/// (fans can surge 1-2s during an RGB change) — this is accepted because RGB
/// changes are rare and user-initiated; the alternative (animations never
/// sticking) is worse.
const RGB_SETTLE: Duration = Duration::from_secs(3);
/// Failsafe engages when a sensor has been unreadable this long — or immediately if it has never produced a reading.
const SENSOR_FAILSAFE_AFTER: Duration = Duration::from_secs(60);

/// Bond classification for a device seen on the air.
#[derive(Debug, Clone, PartialEq)]
pub enum Bond {
    /// Bound to our master (master_mac == link.master_mac).
    Ours,
    /// Bound to a different master (all-nonzero master_mac != ours).
    Foreign,
    /// Not bound to any master (master_mac == [0;6]).
    Unbound,
}

impl Bond {
    pub fn as_str(&self) -> &'static str {
        match self {
            Bond::Ours => "Ours",
            Bond::Foreign => "Foreign",
            Bond::Unbound => "Unbound",
        }
    }
}

/// An entry in the air inventory: a device recently seen via GetDev.
pub struct AirEntry {
    pub record: DeviceRecord,
    pub last_seen: Instant,
    pub bond: Bond,
}

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
    ipc_rx: Option<std::sync::mpsc::Receiver<crate::ipc::IpcCmd>>,
    dongle: Option<Dongle<T>>,
    link: Option<Link>,
    reliability: Reliability,
    curves: HashMap<String, CurveRuntime>,
    devices: HashMap<[u8; 6], DeviceRuntime>,
    last_reconnect: Option<Instant>,
    last_poll: Option<Instant>,
    last_fan_tick: Option<Instant>,
    last_heartbeat: Option<Instant>,
    /// Set after each successful RGB upload; poll_devices, fan_tick, and
    /// send_heartbeat are skipped until this instant passes, giving the
    /// firmware's flash-commit window the RF silence it needs.
    rgb_settle_until: Option<Instant>,
    pub tx_wedged: bool,
    /// Air inventory: every device seen on the air, keyed by MAC. Entries
    /// are updated on every ingest and pruned after 30s of silence.
    pub air: HashMap<[u8; 6], AirEntry>,
}

impl<T: UsbIo> Supervisor<T> {
    pub fn new(
        cfg: Config,
        hwmon_base: PathBuf,
        connector: Box<dyn FnMut() -> llw_protocol::Result<Dongle<T>> + Send>,
        ipc_rx: Option<std::sync::mpsc::Receiver<crate::ipc::IpcCmd>>,
    ) -> Self {
        let reliability = Reliability::new(&cfg.reliability);
        let (curves, devices) = build_runtimes(&cfg);
        Self {
            cfg,
            hwmon_base,
            connector,
            ipc_rx,
            dongle: None,
            link: None,
            reliability,
            curves,
            devices,
            last_reconnect: None,
            last_poll: None,
            last_fan_tick: None,
            last_heartbeat: None,
            rgb_settle_until: None,
            tx_wedged: false,
            air: HashMap::new(),
        }
    }

    /// One pass of everything due at `now`.
    pub fn step(&mut self, now: Instant) -> StepOutcome {
        let mut out = StepOutcome::default();
        self.drain_ipc(now);
        self.ensure_connected(now);
        if self.dongle.is_none() {
            return out;
        }
        if self.link.is_none() {
            out.acquired = self.try_acquire_link(now);
            // Acquisition steps do ONLY acquisition: poll/PWM/heartbeat/RGB
            // and the reliability poll all defer to the next step (50ms in
            // production). Keeps one ingest per step and makes simulation
            // scripts deterministic.
            return out;
        }
        // Check whether the post-upload RF-silence window has expired.
        if self.rgb_settle_until.is_some_and(|u| now >= u) {
            self.rgb_settle_until = None;
        }
        let in_settle = self.rgb_settle_until.is_some();

        // poll_devices, fan_tick, and send_heartbeat are suppressed during
        // the settle window (last_* stamps are NOT updated so they fire
        // naturally once the window closes).
        if !in_settle {
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
        }
        if out.polled {
            out.uploaded_rgb = self.rgb_tick(now);
        }
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
                // Formal backstop: unreachable in current wiring (a successful
                // acquisition clears the escalation streak; failed acquisitions
                // keep the link down, which blocks the reliability poll). Kept
                // for defense in depth.
                out.tier2 = true;
                self.tier2_reconnect(now);
            }
        }
        out
    }

    /// Production loop. 50ms granularity; all real timing lives in step().
    pub fn run(&mut self, shutdown: &std::sync::atomic::AtomicBool) {
        info!("supervisor running");
        if self.cfg.observation.poll_ms < 50 || self.cfg.control.tick_ms < 50 {
            warn!("poll_ms/tick_ms below the 50ms loop granularity are clamped by the step cadence");
        }
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
                    notify_wedge("Lian Li wireless: TX dongle missing — if it stays gone, power-cycle the PSU (known firmware wedge)");
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
                // Reset per-device state BEFORE ingesting the acquisition
                // record: a carried dropout streak would otherwise emit one
                // spurious observation from the acquisition record itself.
                for d in self.devices.values_mut() {
                    d.expected_fx = None;
                    d.last_sent = None;
                    d.filter = DropoutFilter::default();
                }
                self.ingest_records(&records, now);
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
        // Prune air entries that haven't been seen for >30s.
        self.air.retain(|_, entry| now.duration_since(entry.last_seen) < AIR_EXPIRY);
    }

    fn ingest_records(&mut self, records: &[DeviceRecord], now: Instant) {
        let threshold = self.cfg.observation.consecutive_polls;
        // Classify bond based on the link we hold (if any).
        let our_master = self.link.map(|l| l.master_mac);
        for rec in records {
            // Update air inventory for EVERY record, regardless of config.
            let bond = classify_bond(rec, our_master.as_ref());
            self.air
                .entry(rec.mac)
                .and_modify(|e| {
                    e.record = rec.clone();
                    e.last_seen = now;
                    e.bond = bond.clone();
                })
                .or_insert_with(|| AirEntry {
                    record: rec.clone(),
                    last_seen: now,
                    bond: bond.clone(),
                });

            // Configured-device flow: unchanged.
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
            let Ok(mac) = crate::config::parse_mac(&dc.mac) else { continue };
            let Some(dev) = self.devices.get_mut(&mac) else { continue };
            let Some(rec) = dev.last_record.clone() else { continue };

            // Resolve the frames to upload: effect takes precedence over color.
            // Returns (frames, interval_ms). Static color is a single-frame upload.
            let upload_payload: Option<(Vec<Vec<[u8; 3]>>, u16)> =
                if let Some(spec) = &dc.effect {
                    // Effect path: compile via bridge; fall through to color on None.
                    // Frame budget is data-driven from the Task 8 flash probe (byte-based).
                    effects_bridge::compile(spec, &rec, effects_bridge::frame_budget(rec.total_leds()))
                        .or_else(|| {
                            dc.color.map(|color| {
                                (vec![rgb_assert::static_frame(&rec, &color)], 5000)
                            })
                        })
                } else {
                    dc.color.map(|color| (vec![rgb_assert::static_frame(&rec, &color)], 5000))
                };

            let Some((frames, interval_ms)) = upload_payload else { continue };

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
                    &frames,
                    interval_ms,
                    4,
                ) {
                    Ok(idx) => {
                        dev.expected_fx = Some(idx);
                        dev.last_rgb_upload = Some(now);
                        uploads += 1;
                        info!("RGB asserted for {}", rec.mac_str());
                        // Hold RF traffic for RGB_SETTLE so the firmware's
                        // flash-commit window gets the silence it needs.
                        self.rgb_settle_until = Some(now + RGB_SETTLE);
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

    #[allow(dead_code)] // future-facing: used in supervisor tests; llw status will call via IPC, not directly
    pub fn link(&self) -> Option<Link> {
        self.link
    }

    fn drain_ipc(&mut self, _now: Instant) {
        // Bounded drain: at most 8 requests per step keeps the control loop fair.
        // Collect commands first to release the borrow on ipc_rx before calling answer().
        let mut cmds = Vec::new();
        if let Some(rx) = &self.ipc_rx {
            for _ in 0..8 {
                match rx.try_recv() {
                    Ok(cmd) => cmds.push(cmd),
                    Err(_) => break,
                }
            }
        }
        for cmd in cmds {
            let resp = self.answer(cmd.req);
            let _ = cmd.reply.send(resp);
        }
    }

    fn answer(&mut self, req: crate::ipc::Request) -> crate::ipc::ResponseEnvelope {
        use crate::ipc::{AirDeviceStatus, DeviceStatus, LinkStatus, Request, ResponseEnvelope, StatusData};
        match req {
            Request::Ping => ResponseEnvelope::ok(Some(serde_json::json!("pong"))),
            Request::Status => {
                let now = Instant::now();
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
                    air: self
                        .air
                        .values()
                        .map(|e| AirDeviceStatus {
                            mac: mac_str(&e.record.mac),
                            kind: e.record.kind.display_name().into(),
                            bond: e.bond.as_str().to_string(),
                            channel: e.record.channel,
                            fan_count: e.record.fan_count,
                            rpm: e.record.fan_rpms,
                            last_seen_s: now.duration_since(e.last_seen).as_secs(),
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
            Request::SetEffect { mac, effect } => {
                // Validate before touching anything (same rules as config.validate).
                if let Err(e) = crate::config::validate_effect(&effect) {
                    return ResponseEnvelope::err(format!("invalid effect: {e}"));
                }
                // Clone-mutate-save-swap: mutate a clone first so that a save
                // failure leaves self.cfg untouched (no half-state).
                let mut new_cfg = self.cfg.clone();
                let Some(dc) = new_cfg.devices.iter_mut().find(|d| d.mac == mac) else {
                    return ResponseEnvelope::err(format!("unknown device {mac}"));
                };
                dc.effect = Some(effect);
                if let Err(e) = new_cfg.save(&crate::config::default_path()) {
                    return ResponseEnvelope::err(format!("save failed: {e}"));
                }
                // Save succeeded — swap in the new config.
                self.cfg = new_cfg;
                // Force immediate re-assert on next rgb_tick.
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
        let (curves, devices) = build_runtimes(&cfg);
        self.reliability = Reliability::new(&cfg.reliability);
        self.cfg = cfg;
        self.curves = curves;
        self.devices = devices;
        if self.link.is_some() {
            self.reliability.on_acquired(Instant::now());
        }
    }
}

/// Classify a device record's bond based on our current master MAC.
/// - `our_master == None` (no link yet): classify conservatively.
///   All-zero master → Unbound; nonzero → Foreign (we don't know our MAC yet).
fn classify_bond(rec: &DeviceRecord, our_master: Option<&[u8; 6]>) -> Bond {
    let zero_master = rec.master_mac == [0u8; 6];
    if zero_master {
        return Bond::Unbound;
    }
    match our_master {
        Some(m) if &rec.master_mac == m => Bond::Ours,
        _ => Bond::Foreign,
    }
}

fn build_runtimes(cfg: &Config) -> (HashMap<String, CurveRuntime>, HashMap<[u8; 6], DeviceRuntime>) {
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
    (curves, devices)
}

fn mac_str(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn due(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= interval)
}

/// Fires once per wedge EPISODE (re-wedge after recovery re-notifies).
fn notify_wedge(msg: &str) {
    let child = std::process::Command::new("notify-send")
        .arg("llw-daemon")
        .arg(msg)
        .spawn();
    if let Ok(mut c) = child {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
}

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
            effect: None,
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
            None,
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
        assert!(!out.polled && out.sent_pwm == 0 && !out.sent_heartbeat && out.uploaded_rgb == 0);
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
            Box::new(move || {
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
            None,
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

    #[test]
    fn connector_failure_marks_wedge_and_backs_off() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let count = Arc::new(AtomicU32::new(0));
        let count_in = Arc::clone(&count);
        let mut sup: Supervisor<FakeIo> = Supervisor::new(
            test_config(),
            std::env::temp_dir(),
            Box::new(move || {
                count_in.fetch_add(1, Ordering::Relaxed);
                Err(llw_protocol::ProtocolError::DeviceNotFound { vid: 0x0416, pid: 0x8040 })
            }),
            None,
        );
        let t0 = Instant::now();
        // First step at t0: connector called once, wedge asserted.
        let out = sup.step(t0);
        assert_eq!(out, StepOutcome::default());
        assert!(sup.tx_wedged);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        // Within backoff window (t0+1s): no second open attempt.
        let _ = sup.step(t0 + Duration::from_secs(1));
        assert_eq!(count.load(Ordering::Relaxed), 1, "backoff must prevent retry within RECONNECT_INTERVAL");
        assert!(sup.tx_wedged);
        // After RECONNECT_INTERVAL (t0+11s): retry fires, count reaches 2.
        let _ = sup.step(t0 + Duration::from_secs(11));
        assert_eq!(count.load(Ordering::Relaxed), 2, "connector must be retried after RECONNECT_INTERVAL");
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

    /// Build a supervisor with an explicit hwmon_base path.
    fn sim_with_hwmon(
        cfg: Config,
        hwmon_base: std::path::PathBuf,
        rx_script: Vec<Vec<u8>>,
    ) -> (Supervisor<FakeIo>, Instant) {
        let sup = Supervisor::new(
            cfg,
            hwmon_base,
            Box::new(move || {
                let rx = FakeIo::default();
                for r in rx_script.clone() {
                    rx.push_read(r);
                }
                Ok(Dongle::from_parts(FakeIo::default(), Some(rx)))
            }),
            None,
        );
        (sup, Instant::now())
    }

    #[test]
    fn sensor_curve_and_failsafe() {
        use crate::config::{Curve, SensorSpec};

        // Build a tempdir hwmon tree: hwmon0/name = "k10temp", temp1_input = 41300
        let dir = tempfile::tempdir().unwrap();
        let hwmon0 = dir.path().join("hwmon0");
        std::fs::create_dir_all(&hwmon0).unwrap();
        std::fs::write(hwmon0.join("name"), "k10temp\n").unwrap();
        let temp_path = hwmon0.join("temp1_input");
        std::fs::write(&temp_path, "41300\n").unwrap();

        // Config: one Curve("cpu") on slot 0, Percent(0) elsewhere.
        let mut cfg = Config::new();
        cfg.curves.push(Curve {
            name: "cpu".into(),
            sensor: SensorSpec { hwmon_name: "k10temp".into(), input: "temp1_input".into() },
            points: vec![(29.0, 30.0), (52.0, 34.0)],
        });
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [
                SlotSpeed::Curve("cpu".into()),
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
            ],
            color: None,
            effect: None,
        });
        cfg.control.tick_ms = 0; // fan tick every step
        cfg.control.keepalive_ms = 1_000_000; // suppress keepalive during test
        cfg.observation.poll_ms = 0; // poll every step
        cfg.control.sensor_failsafe_percent = 50;

        // Expected PWM at 41.3°C:
        // eval(41.3): ratio = (41.3-29)/(52-29) = 12.3/23 ≈ 0.53478
        // pct = 30 + 0.53478*4 ≈ 32.1391
        // percent_to_pwm(32.1391) = (32.1391*2.55) as u8 = 81.954... as u8 = 81
        // hysteresis: first apply → adopts 81
        // rt.pct = (81 + 0.5) / 2.55 ≈ 31.960...
        // next call: percent_to_pwm(31.960...) = (31.960... * 2.55) as u8 = 81.498... = 81
        let expected_pwm: u8 = 81;

        // Script: acquire + fan-tick poll + post-failsafe polls
        let rec = record_bytes([0; 4], [0; 4]);
        let script = vec![getdev_resp(&[rec]); 20];
        let (mut sup, t0) = sim_with_hwmon(cfg, dir.path().to_path_buf(), script);

        // Step 1: acquire
        let out = sup.step(t0);
        assert!(out.acquired);

        // Step 2: fan-tick step — curve should evaluate to expected_pwm
        let out = sup.step(t0 + Duration::from_secs(1));
        assert!(out.polled);
        let dev = sup.devices.get(&MAC).unwrap();
        assert_eq!(dev.desired[0], expected_pwm, "curve eval at 41.3°C should produce PWM {expected_pwm}");

        // Remove sensor file — stale hold for < 60s
        std::fs::remove_file(&temp_path).unwrap();
        let _out = sup.step(t0 + Duration::from_secs(5));
        let dev = sup.devices.get(&MAC).unwrap();
        assert_eq!(dev.desired[0], expected_pwm, "stale hold: desired unchanged within failsafe window");

        // After >60s of unreadable sensor → failsafe 50% = PWM 127
        let out = sup.step(t0 + Duration::from_secs(70));
        let _ = out;
        let dev = sup.devices.get(&MAC).unwrap();
        assert_eq!(dev.desired[0], 127, "failsafe 50% → PWM 127 after sensor unreadable >60s");
    }

    #[test]
    fn sensor_never_exists_immediate_failsafe() {
        use crate::config::{Curve, SensorSpec};

        // Empty tempdir — no hwmon at all
        let dir = tempfile::tempdir().unwrap();

        let mut cfg = Config::new();
        cfg.curves.push(Curve {
            name: "cpu".into(),
            sensor: SensorSpec { hwmon_name: "k10temp".into(), input: "temp1_input".into() },
            points: vec![(29.0, 30.0), (52.0, 34.0)],
        });
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [
                SlotSpeed::Curve("cpu".into()),
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
            ],
            color: None,
            effect: None,
        });
        cfg.control.tick_ms = 0;
        cfg.control.keepalive_ms = 1_000_000;
        cfg.observation.poll_ms = 0;
        cfg.control.sensor_failsafe_percent = 50;

        let rec = record_bytes([0; 4], [0; 4]);
        let script = vec![getdev_resp(&[rec]); 10];
        let (mut sup, t0) = sim_with_hwmon(cfg, dir.path().to_path_buf(), script);

        // Acquire
        let out = sup.step(t0);
        assert!(out.acquired);

        // Very first fan-tick step with never-seen sensor → immediate failsafe
        let _ = sup.step(t0 + Duration::from_secs(1));
        let dev = sup.devices.get(&MAC).unwrap();
        assert_eq!(dev.desired[0], 127, "sensor never existed → immediate failsafe → PWM 127");
    }

    #[test]
    fn tx_write_failure_drops_link_and_reconnects() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let connects = Arc::new(AtomicU32::new(0));
        let connects_in = Arc::clone(&connects);
        let mut cfg = test_config();
        cfg.control.tick_ms = 0;
        cfg.observation.poll_ms = 0;
        cfg.control.keepalive_ms = 0; // send every step

        let rec = record_bytes([0; 4], [0; 4]);
        let healthy = getdev_resp(&[rec]);

        let mut sup: Supervisor<FakeIo> = Supervisor::new(
            cfg,
            std::env::temp_dir(),
            Box::new(move || {
                let n = connects_in.fetch_add(1, Ordering::Relaxed);
                let tx = FakeIo::default();
                let rx = FakeIo::default();
                if n == 0 {
                    // First dongle: TX has one write error (fires on first PWM/heartbeat send).
                    tx.push_write_err(llw_protocol::ProtocolError::Other(
                        "injected TX failure".into(),
                    ));
                    // RX has enough healthy reads for acquisition.
                    for _ in 0..5 {
                        rx.push_read(healthy.clone());
                    }
                }
                // Later connections: silent (no reads) — stays unacquired.
                Ok(Dongle::from_parts(tx, Some(rx)))
            }),
            None,
        );
        let t0 = Instant::now();

        // Step 1: acquire (RX-only — TX write error has not fired yet).
        let out = sup.step(t0);
        assert!(out.acquired, "acquisition must succeed (RX-only path)");
        assert!(sup.link().is_some());

        // Step 2: fan-tick PWM send fires the TX write error → drop_dongle.
        let out = sup.step(t0 + Duration::from_secs(1));
        let _ = out;
        assert!(sup.link().is_none(), "TX failure must drop the link");
        assert_eq!(connects.load(Ordering::Relaxed), 1, "only one connect so far");

        // Step 3: past RECONNECT_INTERVAL → connector called again.
        let _ = sup.step(t0 + Duration::from_secs(12));
        assert_eq!(connects.load(Ordering::Relaxed), 2, "must reconnect after TX failure");
    }

    #[test]
    fn steady_state_keepalive() {
        // Config: Percent(40) all slots, keepalive 10s, tick=0, poll=0.
        let mut cfg = test_config();
        cfg.control.tick_ms = 0;
        cfg.control.keepalive_ms = 10_000;
        cfg.observation.poll_ms = 0;

        // Healthy records: readback already matches [102, 102, 102, 0] so drift never triggers.
        let rec = record_bytes([102, 102, 102, 0], [0; 4]);
        let script = vec![getdev_resp(&[rec]); 20];
        let (mut sup, t0) = sim(cfg, script);

        // Acquire
        let out = sup.step(t0);
        assert!(out.acquired);

        // First fan-tick step: last_sent is None → sends (anchors keepalive timer).
        let out = sup.step(t0 + Duration::from_secs(1));
        assert_eq!(out.sent_pwm, 1, "initial send because last_sent is None");

        // Steps at +2s..+9s: keepalive not due, readback matches → no send.
        for s in 2u64..=9 {
            let out = sup.step(t0 + Duration::from_secs(s));
            assert_eq!(out.sent_pwm, 0, "no send at +{s}s (within keepalive window)");
        }

        // Step at +11s: past keepalive (10s from last send at +1s) → send.
        let out = sup.step(t0 + Duration::from_secs(11));
        assert_eq!(out.sent_pwm, 1, "keepalive fires at +11s");
    }

    #[test]
    fn effect_ripple_upload_fires_on_acquisition_poll() {
        // Verify that a config with effect=ripple causes an RGB upload on the first
        // poll step after acquisition (uploaded_rgb == 1), exactly like the static path.
        // The multi-frame payload is verified separately in effect_ripple_compile_produces_multiframe_upload.
        use llw_effects::{Direction, EffectKind, EffectSpec};

        fn run_sim_and_get_uploaded_rgb(cfg: Config) -> u32 {
            let rec = record_bytes([0; 4], [0; 4]);
            let mut cfg2 = cfg;
            cfg2.observation.poll_ms = 0;
            cfg2.control.tick_ms = 10_000;     // suppress PWM ticks
            cfg2.control.keepalive_ms = 10_000; // suppress keepalive
            let script = vec![getdev_resp(&[rec]); 4];
            let (mut sup, t0) = sim(cfg2, script);
            let out1 = sup.step(t0);
            assert!(out1.acquired, "must acquire");
            let out2 = sup.step(t0 + Duration::from_secs(1));
            out2.uploaded_rgb
        }

        // Static white config
        let mut static_cfg = Config::new();
        static_cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0)],
            color: Some(StaticColor { rgb: [255, 255, 255], brightness: 4 }),
            effect: None,
        });

        // Ripple config (effect takes precedence; no color field needed)
        let mut ripple_cfg = Config::new();
        ripple_cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0)],
            color: None,
            effect: Some(EffectSpec {
                kind: EffectKind::Ripple,
                colors: vec![[0, 0, 255], [136, 0, 255]],
                speed: 3,
                direction: Direction::Forward,
                brightness: 4,
            }),
        });

        // Both paths must produce exactly one upload on the first poll step.
        assert_eq!(run_sim_and_get_uploaded_rgb(static_cfg), 1, "static must upload once");
        assert_eq!(run_sim_and_get_uploaded_rgb(ripple_cfg), 1, "ripple must upload once");
    }

    #[test]
    fn effect_ripple_compile_produces_multiframe_upload() {
        // Verify via effects_bridge directly that the SL-INF 3-fan record compiles
        // to exactly 24 frames of 132 LEDs each — confirming the supervisor will
        // pass a 24-element slice to upload_rgb (not the 1-element static case).
        use crate::effects_bridge;
        use llw_protocol::record::parse_device_record;
        use llw_effects::{Direction, EffectKind, EffectSpec};

        // Construct a synthetic SL-INF 3-fan DeviceRecord identical to the sim record.
        let mut raw = [0u8; 42];
        raw[0..6].copy_from_slice(&MAC);
        raw[6..12].copy_from_slice(&MASTER);
        raw[12] = 2;
        raw[13] = 1;
        raw[18] = 0;  // fan device
        raw[19] = 3;  // 3 fans
        raw[24] = 36; // SL-INF fan type byte (leds_per_fan=44)
        raw[41] = 0x1C;
        let rec = parse_device_record(&raw, 0).expect("valid record");
        assert_eq!(rec.total_leds(), 132);

        let spec = EffectSpec {
            kind: EffectKind::Ripple,
            colors: vec![[0, 0, 255], [136, 0, 255]],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        };

        // Pass explicit 24 so the golden expectations stay valid; the supervisor
        // now calls frame_budget(rec.total_leds()) = 70 for 132 LEDs, but this
        // test exercises the compile path, not the budget formula.
        let (frames, interval_ms) = effects_bridge::compile(&spec, &rec, 24)
            .expect("SL-INF 3-fan ripple must compile");

        // 24 frames, each 132 LEDs — this is what the supervisor passes to upload_rgb.
        assert_eq!(frames.len(), 24, "ripple must produce 24 frames");
        assert!(frames.iter().all(|f| f.len() == 132), "each frame must have 132 LEDs");

        // interval = 3000 / 24 = 125ms (period_ms(3) / 24)
        assert_eq!(interval_ms, 125, "interval should be 125ms");

        // Multi-frame upload has MORE data than single-frame static:
        // static = 1 frame × 132 LEDs; ripple = 24 frames × 132 LEDs (24× more raw data).
        // Verify the frame count difference directly.
        let static_frame_count = 1usize;
        let ripple_frame_count = frames.len();
        assert!(
            ripple_frame_count > static_frame_count,
            "ripple must produce more frames than static (got {ripple_frame_count} vs {static_frame_count})"
        );
        // The 24-frame animation is genuinely multi-frame.
        assert!(ripple_frame_count >= 24, "ripple must be at least 24 frames");
    }

    /// RGB settle window: after a successful upload the supervisor must NOT
    /// send any RF traffic (polls, PWM, heartbeat) for RGB_SETTLE (3s).
    ///
    /// Uses poll_ms=0 / tick_ms=0 so that the suppression — not the interval
    /// gating — is provably the cause of the silence.
    #[test]
    fn rgb_settle_window_suppresses_rf_traffic() {
        // Static-color config so rgb_tick fires on the first poll.
        let mut cfg = test_config(); // color: Some(white)
        cfg.observation.poll_ms = 0; // poll every step
        cfg.control.tick_ms = 0; // fan tick every step
        cfg.control.keepalive_ms = 0; // heartbeat every step (per-step minimum)

        // Healthy record: fx=[0;4] so expected_fx=None → upload on first poll.
        let rec = record_bytes([102, 102, 102, 0], [0; 4]);
        // Enough reads for acquisition + many post-upload steps.
        let script = vec![getdev_resp(&[rec]); 20];
        let (mut sup, t0) = sim(cfg, script);

        // Step 1 — connect + acquire.
        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");

        // Step 2 (T = t0+1s) — first poll: RGB uploads, settle window opens.
        let t_upload = t0 + Duration::from_secs(1);
        let out = sup.step(t_upload);
        assert!(out.polled, "must poll on step 2");
        assert_eq!(out.uploaded_rgb, 1, "must upload RGB at T");
        // Fan tick and heartbeat may also fire on this step (settle not yet active).

        // Steps within (T, T+3s) — settle window active: no polls, no PWM, no heartbeat.
        for ms in [500u64, 1000, 1500, 2000, 2500, 2900] {
            let now = t_upload + Duration::from_millis(ms);
            let out = sup.step(now);
            assert!(
                !out.polled,
                "poll must be suppressed at T+{ms}ms (settle window)"
            );
            assert_eq!(
                out.sent_pwm, 0,
                "PWM must be suppressed at T+{ms}ms (settle window)"
            );
            assert!(
                !out.sent_heartbeat,
                "heartbeat must be suppressed at T+{ms}ms (settle window)"
            );
            assert_eq!(
                out.uploaded_rgb, 0,
                "no RGB re-upload during settle window at T+{ms}ms"
            );
        }

        // Step at T+3.5s — settle window expired: polls and PWM resume.
        // (heartbeat interval is 1s; last_heartbeat was set at T or earlier,
        // so ≥3.5s later it is also due.)
        let out = sup.step(t_upload + Duration::from_millis(3500));
        assert!(out.polled, "poll must resume after settle window (T+3500ms)");
        assert!(
            out.sent_pwm > 0 || out.sent_heartbeat,
            "PWM or heartbeat must resume after settle window (T+3500ms)"
        );
    }

    // ── Air inventory tests ───────────────────────────────────────────────────

    /// Build a 42-byte record with a custom MAC and master_mac for air tests.
    fn air_record_bytes(mac: [u8; 6], master: [u8; 6], pwm: [u8; 4]) -> [u8; 42] {
        let mut r = [0u8; 42];
        r[0..6].copy_from_slice(&mac);
        r[6..12].copy_from_slice(&master);
        r[12] = 2;   // channel
        r[13] = 1;   // rx_type
        r[19] = 3;   // fan_count
        r[20..24].copy_from_slice(&[0u8; 4]);
        r[24] = 36;  // SL-INF fan type
        r[36..40].copy_from_slice(&pwm);
        r[41] = 0x1C;
        r
    }

    /// Build a GetDev response with multiple arbitrary records.
    fn getdev_resp_multi(records: &[[u8; 42]]) -> Vec<u8> {
        let mut resp = vec![0u8; 4 + 42 * records.len()];
        resp[0] = 0x10;
        resp[1] = records.len() as u8;
        resp[2] = 0x80;
        for (i, r) in records.iter().enumerate() {
            resp[4 + i * 42..4 + (i + 1) * 42].copy_from_slice(r);
        }
        resp
    }

    /// Air inventory: foreign + unbound records alongside the configured SL-INF.
    ///
    /// After acquisition + one poll the air inventory must contain exactly 3 entries:
    /// - configured SL-INF (MAC) → Ours
    /// - a foreign device       → Foreign
    /// - an unbound device      → Unbound
    #[test]
    fn air_inventory_classifies_ours_foreign_unbound() {
        let foreign_mac: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01];
        let unbound_mac: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x02];
        let foreign_master: [u8; 6] = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let zero_master: [u8; 6] = [0u8; 6];

        // Records: configured SL-INF (MASTER), a foreign device (foreign_master), an unbound device (zero master)
        let rec_ours = air_record_bytes(MAC, MASTER, [102, 102, 102, 0]);
        let rec_foreign = air_record_bytes(foreign_mac, foreign_master, [80; 4]);
        let rec_unbound = air_record_bytes(unbound_mac, zero_master, [0; 4]);

        let mut cfg = test_config();
        cfg.observation.poll_ms = 0;
        cfg.control.tick_ms = 10_000;
        cfg.control.keepalive_ms = 10_000;

        // Script: acquire (all three records) + one poll step (same records)
        let combined = getdev_resp_multi(&[rec_ours, rec_foreign, rec_unbound]);
        let script = vec![combined; 3];
        let (mut sup, t0) = sim(cfg, script);

        // Step 1: acquire
        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");
        // Air is updated during acquisition ingest too
        assert_eq!(sup.air.len(), 3, "air must have 3 entries after acquisition");

        // Step 2: poll step
        let _ = sup.step(t0 + Duration::from_secs(1));
        assert_eq!(sup.air.len(), 3, "air must still have 3 entries after poll");

        // Validate bond classifications
        let ours_entry = sup.air.get(&MAC).expect("configured device must be in air");
        assert_eq!(ours_entry.bond, Bond::Ours, "configured SL-INF must be Ours");

        let foreign_entry = sup.air.get(&foreign_mac).expect("foreign device must be in air");
        assert_eq!(foreign_entry.bond, Bond::Foreign, "device with different master must be Foreign");

        let unbound_entry = sup.air.get(&unbound_mac).expect("unbound device must be in air");
        assert_eq!(unbound_entry.bond, Bond::Unbound, "device with zero master must be Unbound");
    }

    /// Air inventory expiry: an entry that stops appearing must be pruned after 30s.
    ///
    /// Script: acquire + a few polls with the foreign device → then polls without
    /// it → 30s later the entry is gone.
    ///
    /// Uses a color-free config to avoid the RGB settle window suppressing polls
    /// (settle window would prevent poll_devices from running and defer expiry).
    #[test]
    fn air_inventory_expires_unseen_entries() {
        let foreign_mac: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01];
        let foreign_master: [u8; 6] = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];

        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_foreign = air_record_bytes(foreign_mac, foreign_master, [0; 4]);

        // No color → no RGB upload → no RGB settle window suppressing polls.
        let mut cfg = Config::new();
        cfg.devices.push(crate::config::DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [
                crate::config::SlotSpeed::Percent(0),
                crate::config::SlotSpeed::Percent(0),
                crate::config::SlotSpeed::Percent(0),
                crate::config::SlotSpeed::Percent(0),
            ],
            color: None,
            effect: None,
        });
        cfg.observation.poll_ms = 0;  // poll every step
        cfg.control.tick_ms = 10_000; // suppress fan tick
        cfg.control.keepalive_ms = 10_000; // suppress keepalive

        // Script:
        //   1 × both devices (acquisition)
        //   2 × both devices (polls while foreign is visible)
        //   10 × only our device (foreign gone from air)
        let with_foreign = getdev_resp_multi(&[rec_ours, rec_foreign]);
        let without_foreign = getdev_resp_multi(&[rec_ours]);
        let mut script = vec![with_foreign; 3];
        script.extend(vec![without_foreign; 10]);
        let (mut sup, t0) = sim(cfg, script);

        // Acquire at t0 (consumes script[0] = with_foreign)
        let out = sup.step(t0);
        assert!(out.acquired);
        assert_eq!(sup.air.len(), 2, "both devices on air at acquisition");

        // Step at +1s: poll with both (consumes script[1] = with_foreign)
        let _ = sup.step(t0 + Duration::from_secs(1));
        // Step at +2s: poll with both — foreign last_seen = t0+2s (consumes script[2] = with_foreign)
        let t_last_foreign = t0 + Duration::from_secs(2);
        let _ = sup.step(t_last_foreign);
        assert_eq!(sup.air.len(), 2, "both entries after last foreign poll");

        // Step at +3s: poll WITHOUT foreign — foreign still in air (< 30s)
        // (consumes script[3] = first without_foreign)
        let _ = sup.step(t0 + Duration::from_secs(3));
        assert_eq!(sup.air.len(), 2, "foreign still in air at +1s after last seen");

        // Step at t_last_foreign + 31s = t0+33s: foreign must be pruned
        // (consumes script[4] = second without_foreign; prune runs with 31s elapsed)
        let t_expired = t_last_foreign + Duration::from_secs(31);
        let _ = sup.step(t_expired);
        assert_eq!(sup.air.len(), 1, "foreign entry must expire after 30s unseen");
        assert!(sup.air.contains_key(&MAC), "configured device must remain");
        assert!(!sup.air.contains_key(&foreign_mac), "foreign device must be pruned");
    }

    /// Configured-device (Ours) flow is byte-identical: existing sim still green
    /// when air entries are present. This verifies no regression via the
    /// healthy_loop_acquires_and_commands semantics with air inventory active.
    #[test]
    fn air_inventory_ours_flow_unchanged() {
        let rec = record_bytes([0; 4], [0; 4]);
        let script = vec![getdev_resp(&[rec]); 6];
        let (mut sup, t0) = sim(test_config(), script);

        let out = sup.step(t0);
        assert!(out.acquired);
        assert_eq!(sup.air.len(), 1, "configured device must appear in air");

        let out = sup.step(t0 + Duration::from_millis(1100));
        assert!(out.polled);
        assert_eq!(out.sent_pwm, 1, "PWM send must be unchanged");

        // The configured device in air must be Ours
        let entry = sup.air.get(&MAC).expect("configured device in air");
        assert_eq!(entry.bond, Bond::Ours, "configured device must be Ours in air");

        // The device runtime must have last_record set (existing behaviour)
        let dev = sup.devices.get(&MAC).unwrap();
        assert!(dev.last_record.is_some(), "last_record must still be updated");
    }
}
