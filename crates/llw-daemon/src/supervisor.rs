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

/// Surge notifications at most this often (journal WARNs are unthrottled).
const SURGE_NOTIFY_COOLDOWN: Duration = Duration::from_secs(60);

/// PWM frames per send while the master is REVERTED (readback all-zero,
/// commanded nonzero) — repetition beats the RF noise that caused the revert.
const REVERT_BURST_REPEATS: usize = 4;
const REVERT_BURST_GAP: Duration = Duration::from_millis(25);

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
#[derive(Clone)]
pub struct AirEntry {
    pub record: DeviceRecord,
    pub last_seen: Instant,
    pub bond: Bond,
}

const BIND_DEADLINE: Duration = Duration::from_secs(5);
const BIND_FAIL_CLEAR: Duration = Duration::from_secs(30);

pub(crate) struct BindOp {
    pub mac: [u8; 6],
    pub unbind: bool,
    pub target_rx: u8,
    pub deadline: Instant,
    pub bursts: u8,
    pub failed_at: Option<Instant>,
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
    /// Surge watchdog (2026-07-17 incident): judges zero-readback windows
    /// plus the fan-inertia tail against the healthy RPM baseline.
    surge: crate::observation::SurgeTracker,
    /// Stall watchdog (2026-07-20 incident): commanded fans with all tachs
    /// at zero — the thermally dangerous, previously invisible failure shape.
    stall: crate::observation::StallTracker,
    /// rf[16] for PWM commands: 1-based position of this device among OUR
    /// master's devices in the latest GetDev report (upstream fan_speed.rs
    /// semantics — the raw GetDev slot index is WRONG when foreign/master
    /// records occupy earlier slots; port-fidelity audit 2026-07-17).
    seq_index: u8,
}

pub struct Supervisor<T: UsbIo> {
    cfg: Config,
    config_path: PathBuf,
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
    /// Set after a successful RGB upload or bind/unbind; poll_devices, fan_tick, and
    /// send_heartbeat are skipped until this instant passes, giving the
    /// firmware's flash-commit window the RF silence it needs.
    rf_settle_until: Option<Instant>,
    /// Rate limit for surge desktop notifications.
    last_surge_notify: Option<Instant>,
    pub tx_wedged: bool,
    /// Air inventory: every device seen on the air, keyed by MAC. Entries
    /// are updated on every ingest and pruned after 30s of silence.
    pub air: HashMap<[u8; 6], AirEntry>,
    /// In-flight bind or unbind operation (one at a time).
    pub pending_op: Option<BindOp>,
}

impl<T: UsbIo> Supervisor<T> {
    pub fn new(
        cfg: Config,
        hwmon_base: PathBuf,
        connector: Box<dyn FnMut() -> llw_protocol::Result<Dongle<T>> + Send>,
        ipc_rx: Option<std::sync::mpsc::Receiver<crate::ipc::IpcCmd>>,
        config_path: PathBuf,
    ) -> Self {
        let reliability = Reliability::new(&cfg.reliability);
        let (curves, devices) = build_runtimes(&cfg);
        Self {
            cfg,
            config_path,
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
            rf_settle_until: None,
            last_surge_notify: None,
            tx_wedged: false,
            air: HashMap::new(),
            pending_op: None,
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
        if self.rf_settle_until.is_some_and(|u| now >= u) {
            self.rf_settle_until = None;
        }
        let in_settle = self.rf_settle_until.is_some();

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
        // Bind/unbind convergence polling: runs even during the RF settle window
        // so that bind convergence is not blocked by a prior RGB upload.
        if in_settle && self.pending_op.is_some()
            && due(self.last_poll, now, Duration::from_millis(self.cfg.observation.poll_ms))
        {
            self.last_poll = Some(now);
            out.polled = true;
            self.poll_devices(now);
        }
        if self.pending_op.is_some() {
            self.check_bind_convergence(now);
        }
        // RF-silence invariant: re-read rf_settle_until instead of the stale
        // `in_settle` snapshot — this covers both the forced-poll-during-settle
        // case (pending op above sets out.polled while in settle) and same-step
        // settle engagement (check_bind_convergence just sent save_config and
        // opened the window). No RGB uploads while the window is open.
        if out.polled && self.rf_settle_until.is_none() {
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
                //
                // `expected_fx` is deliberately KEPT: the drift guard compares
                // it against the fresh record's effect index, so an intact
                // onboard animation survives re-acquisition without a
                // re-upload (each upload is a device flash write — Tier-1
                // storms under RF interference burned hundreds of them,
                // 2026-07-17). Config changes still force re-assert via the
                // SetEffect/SetColor/apply_config invalidation paths.
                for d in self.devices.values_mut() {
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
        // Surges judged this ingest: (mac string, judged surge).
        let mut surges: Vec<(String, crate::observation::Surge)> = Vec::new();
        // Stall transitions this ingest: (mac string, Some(polls)=ended / None=started).
        let mut stalls: Vec<(String, Option<u32>)> = Vec::new();
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
            if let Some(ours) = our_master.as_ref() {
                dev.seq_index = records
                    .iter()
                    .filter(|r| &r.master_mac == ours && r.device_type != 0xFF)
                    .position(|r| r.mac == rec.mac)
                    .map(|i| (i + 1) as u8)
                    .unwrap_or(1);
            }
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

            // Surge watchdog: the physical peak arrives AFTER readback
            // recovers (fan inertia — 4Hz captures, 2026-07-17), so the
            // tracker judges the window plus a post-recovery tail.
            let max_rpm = rec
                .fan_rpms
                .iter()
                .take(rec.fan_count as usize)
                .copied()
                .max()
                .unwrap_or(0);
            // poll_ms 0 = unthrottled (test convention) — use the 1s default
            // for tail sizing so sims and slow configs agree.
            let poll_ms = match self.cfg.observation.poll_ms {
                0 => 1000,
                ms => ms,
            };
            let tail = crate::observation::tail_polls_for(poll_ms);
            if let Some(surge) = dev.surge.observe(commanded, readback_zero, max_rpm, tail) {
                surges.push((rec.mac_str(), surge));
            }

            // Stall watchdog: commanded but every tach reads zero. Alarm the
            // moment the stall is confirmed, and log its length when it ends.
            let all_rpm_zero = rec.fan_count > 0
                && rec
                    .fan_rpms
                    .iter()
                    .take(rec.fan_count as usize)
                    .all(|&r| r == 0);
            let was_stalled = dev.stall.stalled();
            if let Some(ended) = dev.stall.observe(commanded, all_rpm_zero) {
                stalls.push((rec.mac_str(), Some(ended.polls)));
            } else if !was_stalled && dev.stall.stalled() {
                stalls.push((rec.mac_str(), None));
            }

            dev.last_record = Some(rec.clone());
        }
        for (mac, transition) in stalls {
            match transition {
                None => {
                    warn!("FAN STALL on {mac}: fans commanded but every tach reads 0");
                    self.reliability.on_stall();
                    notify_stall(&mac);
                }
                Some(polls) => {
                    warn!("fan stall on {mac} ended after {polls} polls");
                }
            }
        }
        for (mac, surge) in surges {
            warn!(
                "fan surge on {mac}: peak {} rpm (baseline {}) across a dropout window + inertia tail",
                surge.peak_rpm, surge.baseline_rpm
            );
            self.reliability.on_surge(surge.peak_rpm);
            let due = self
                .last_surge_notify
                .is_none_or(|t| now.duration_since(t) >= SURGE_NOTIFY_COOLDOWN);
            if due {
                self.last_surge_notify = Some(now);
                notify_surge(surge.peak_rpm, surge.baseline_rpm);
            }
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
            // A material commanded-speed change makes RPM movement legitimate;
            // abort any in-flight surge episode (tracker caller contract).
            if dev
                .desired
                .iter()
                .zip(pwm.iter())
                .any(|(a, b)| a.abs_diff(*b) > 10)
            {
                dev.surge.reset();
            }
            dev.desired = pwm;
            if fan::should_send(&pwm, &rec.current_pwm, dev.last_sent, now, keepalive) {
                let rf = pwm_frame(
                    &mac,
                    &link.master_mac,
                    rec.rx_type,
                    link.channel,
                    dev.seq_index,
                    &pwm,
                );
                // Firmware-revert recovery (2026-07-17): when the master has
                // reverted (readback all-zero while commanded), single frames
                // die to the same RF noise that caused the revert — burst-
                // repeat so one lands (burst-recovery probe: 3-12 frames at
                // 30ms recovered in 0.3-1.2s; single-per-tick took 2-4s of
                // full-speed fans).
                let reverted = pwm.iter().take(rec.fan_count as usize).any(|&p| p > 0)
                    && rec
                        .current_pwm
                        .iter()
                        .take(rec.fan_count as usize)
                        .all(|&p| p == 0);
                let repeats = if reverted { REVERT_BURST_REPEATS } else { 1 };
                let Some(dongle) = self.dongle.as_mut() else { return sent };
                let mut failed = false;
                for i in 0..repeats {
                    if let Err(e) = dongle.send_rf_frame(&rf, rec.channel, rec.rx_type) {
                        warn!("PWM send failed for {}: {e}", rec.mac_str());
                        failed = true;
                        break;
                    }
                    if i + 1 < repeats {
                        std::thread::sleep(REVERT_BURST_GAP);
                    }
                }
                if failed {
                    self.drop_dongle();
                    return sent;
                }
                dev.last_sent = Some(now);
                sent += 1;
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
                        self.rf_settle_until = Some(now + RGB_SETTLE);
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

    #[cfg(test)]
    pub(crate) fn telemetry(&self) -> crate::reliability::Telemetry {
        self.reliability.telemetry()
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
        use crate::ipc::{AirDeviceStatus, CurveStatus, DeviceStatus, LinkStatus, ListSensorsData, PendingOpStatus, Request, ResponseEnvelope, StatusData};
        match req {
            Request::Ping => ResponseEnvelope::ok(Some(serde_json::json!("pong"))),
            Request::Status => {
                let now = Instant::now();
                let pending = self.pending_op.as_ref().map(|op| PendingOpStatus {
                    op: if op.unbind { "unbind".to_string() } else { "bind".to_string() },
                    mac: mac_str(&op.mac),
                    state: if op.failed_at.is_some() { "failed".to_string() } else { "converging".to_string() },
                });
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
                    pending,
                    // Config order (deterministic, unlike the runtime HashMap);
                    // the EMA'd value the fan controller uses, None until the
                    // sensor has produced a plausible reading.
                    curves: self
                        .cfg
                        .curves
                        .iter()
                        .map(|c| CurveStatus {
                            name: c.name.clone(),
                            sensor_c: self.curves.get(&c.name).and_then(|rt| rt.ema.value()),
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
                    if let Err(e) = config.save(&self.config_path) {
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
                if let Err(e) = self.cfg.save(&self.config_path) {
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
                if let Err(e) = new_cfg.save(&self.config_path) {
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
            Request::Bind { mac } => {
                // No new RF ops while the post-save/post-upload silence window
                // is open — the firmware's flash-commit needs the quiet.
                if self.rf_settle_until.is_some() {
                    return ResponseEnvelope::err("radio settling — retry shortly");
                }
                let Some(link) = self.link else {
                    return ResponseEnvelope::err("no link");
                };
                let Ok(mac_bytes) = crate::config::parse_mac(&mac) else {
                    return ResponseEnvelope::err(format!("invalid MAC: {mac}"));
                };
                let Some(air_entry) = self.air.get(&mac_bytes) else {
                    return ResponseEnvelope::err("not visible on air");
                };
                match &air_entry.bond {
                    Bond::Foreign => return ResponseEnvelope::err("bound to another controller — unbind it there first"),
                    Bond::Ours => return ResponseEnvelope::err("already bound"),
                    Bond::Unbound => {}
                }
                if let Some(op) = &self.pending_op {
                    if op.failed_at.is_none() {
                        return ResponseEnvelope::err("operation already in progress");
                    }
                }
                // Compute target_rx from all air records
                let air_records: Vec<_> = self.air.values().map(|e| e.record.clone()).collect();
                let target_rx = llw_protocol::record::unused_rx(&air_records, &link.master_mac);
                let current_pwm = air_entry.record.current_pwm;
                let current_channel = air_entry.record.channel;
                let current_rx = air_entry.record.rx_type;
                let frame = llw_protocol::frames::bind_frame(
                    &mac_bytes,
                    &link.master_mac,
                    target_rx,
                    link.channel,
                    &current_pwm,
                );
                let now = Instant::now();
                if let Some(dongle) = self.dongle.as_mut() {
                    if let Err(e) = dongle.send_bind_burst(&frame, current_channel, current_rx) {
                        return ResponseEnvelope::err(format!("bind burst failed: {e}"));
                    }
                }
                self.pending_op = Some(BindOp {
                    mac: mac_bytes,
                    unbind: false,
                    target_rx,
                    deadline: now + BIND_DEADLINE,
                    bursts: 1,
                    failed_at: None,
                });
                ResponseEnvelope::ok(Some(serde_json::json!({"state": "started"})))
            }
            Request::Unbind { mac } => {
                if self.rf_settle_until.is_some() {
                    return ResponseEnvelope::err("radio settling — retry shortly");
                }
                let Some(link) = self.link else {
                    return ResponseEnvelope::err("no link");
                };
                let Ok(mac_bytes) = crate::config::parse_mac(&mac) else {
                    return ResponseEnvelope::err(format!("invalid MAC: {mac}"));
                };
                let Some(air_entry) = self.air.get(&mac_bytes) else {
                    return ResponseEnvelope::err("not visible on air");
                };
                match &air_entry.bond {
                    Bond::Ours => {}
                    Bond::Foreign => return ResponseEnvelope::err("device is not bound to this controller"),
                    Bond::Unbound => return ResponseEnvelope::err("device is not bound"),
                }
                if let Some(op) = &self.pending_op {
                    if op.failed_at.is_none() {
                        return ResponseEnvelope::err("operation already in progress");
                    }
                }
                let current_pwm = air_entry.record.current_pwm;
                let current_channel = air_entry.record.channel;
                let current_rx = air_entry.record.rx_type;
                let zero_master = [0u8; 6];
                let frame = llw_protocol::frames::bind_frame(
                    &mac_bytes,
                    &zero_master,
                    0,
                    link.channel,
                    &current_pwm,
                );
                let now = Instant::now();
                if let Some(dongle) = self.dongle.as_mut() {
                    if let Err(e) = dongle.send_bind_burst(&frame, current_channel, current_rx) {
                        return ResponseEnvelope::err(format!("unbind burst failed: {e}"));
                    }
                }
                self.pending_op = Some(BindOp {
                    mac: mac_bytes,
                    unbind: true,
                    target_rx: 0,
                    deadline: now + BIND_DEADLINE,
                    bursts: 1,
                    failed_at: None,
                });
                ResponseEnvelope::ok(Some(serde_json::json!({"state": "started"})))
            }
            Request::ListSensors => match sensors::enumerate(&self.hwmon_base) {
                Ok(sensors) => match serde_json::to_value(&ListSensorsData { sensors }) {
                    Ok(v) => ResponseEnvelope::ok(Some(v)),
                    Err(e) => ResponseEnvelope::err(e.to_string()),
                },
                Err(e) => ResponseEnvelope::err(e.to_string()),
            },
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

    /// Called every step when a bind/unbind op is pending.
    /// Checks the air inventory for convergence, re-bursts once on deadline,
    /// and marks the op failed after the second deadline.
    fn check_bind_convergence(&mut self, now: Instant) {
        let Some(op) = self.pending_op.as_ref() else { return };

        // Clear failed ops after BIND_FAIL_CLEAR (30s)
        if let Some(failed_at) = op.failed_at {
            if now.duration_since(failed_at) >= BIND_FAIL_CLEAR {
                self.pending_op = None;
            }
            return;
        }

        let mac = op.mac;
        let unbind = op.unbind;
        let target_rx = op.target_rx;

        // Check convergence from air inventory
        let converged = if unbind {
            match self.air.get(&mac) {
                // Absent counts as converged for completeness, but real
                // hardware keeps advertising with a zeroed master after an
                // unbind, so convergence in practice arrives via the Unbound
                // reclassification on the next poll. A silently vanished
                // device only leaves the inventory via the 30s air prune —
                // long after both 5s deadlines — so it fails the op instead
                // (hardware confirmation tracked in Task 6).
                None => true,
                Some(e) => e.bond == Bond::Unbound,
            }
        } else {
            match self.air.get(&mac) {
                Some(e) => e.bond == Bond::Ours && e.record.rx_type == target_rx,
                None => false,
            }
        };

        if converged {
            let op = self.pending_op.take().unwrap();
            let Some(link) = self.link else {
                warn!(
                    "bind op for {} converged but the link is gone — dropping op without save-config",
                    mac_str(&op.mac)
                );
                return;
            };
            // Send save_config
            if let Some(dongle) = self.dongle.as_mut() {
                if let Err(e) = dongle.send_save_config(&link.master_mac, link.channel) {
                    warn!("send_save_config failed after bind: {e}");
                }
            }
            // Engage RF settle window to protect the flash-commit
            self.rf_settle_until = Some(now + RGB_SETTLE);

            let mac_str = mac_str(&op.mac);
            if unbind {
                self.cfg.devices.retain(|d| d.mac != mac_str);
                self.devices.remove(&op.mac);
                if let Err(e) = self.cfg.save(&self.config_path) {
                    warn!("config save after unbind failed: {e}");
                }
                info!("unbind complete for {mac_str}");
            } else {
                let first_curve = self.cfg.curves.first().map(|c| c.name.clone());
                let default_slot = match first_curve {
                    Some(name) => SlotSpeed::Curve(name),
                    None => SlotSpeed::Percent(40),
                };
                // Re-bind of a still-configured device keeps the user's
                // existing slots/colors — only add a default entry when none
                // exists. (The runtime insert below replaces any stale
                // DeviceRuntime, which is correct: a fresh dropout filter
                // after the device re-joined the network.)
                if !self.cfg.devices.iter().any(|d| d.mac == mac_str) {
                    self.cfg.devices.push(crate::config::DeviceConfig {
                        mac: mac_str.clone(),
                        name: None,
                        slots: [
                            default_slot.clone(),
                            default_slot.clone(),
                            default_slot.clone(),
                            default_slot,
                        ],
                        color: None,
                        effect: None,
                    });
                }
                if let Err(e) = self.cfg.save(&self.config_path) {
                    warn!("config save after bind failed: {e}");
                }
                self.devices.insert(
                    op.mac,
                    DeviceRuntime {
                        mac: op.mac,
                        desired: [0; 4],
                        last_sent: None,
                        filter: crate::observation::DropoutFilter::default(),
                        expected_fx: None,
                        last_rgb_upload: None,
                        last_record: None,
                        surge: Default::default(),
                        stall: Default::default(),
                        seq_index: 1,
                    },
                );
                info!("bind complete for {mac_str}");
            }
            return;
        }

        // Check deadline — re-burst once, then mark failed
        let op = self.pending_op.as_mut().unwrap();
        if now >= op.deadline {
            if op.bursts < 2 {
                // CRITICAL invariant (re-burst gate): never transmit a bind
                // frame at a device whose bond no longer matches the op's
                // precondition — Unbound for bind, Ours for unbind. A device
                // that went Foreign mid-op (another controller claimed it),
                // flipped state, or vanished from the air fails the op
                // immediately, burst-free.
                let precondition_holds = match self.air.get(&mac) {
                    Some(e) if unbind => e.bond == Bond::Ours,
                    Some(e) => e.bond == Bond::Unbound,
                    None => false,
                };
                if !precondition_holds {
                    warn!(
                        "{} op for {} lost its bond precondition — failing without re-burst",
                        if unbind { "unbind" } else { "bind" },
                        mac_str(&mac)
                    );
                    self.pending_op.as_mut().unwrap().failed_at = Some(now);
                    return;
                }
                // Re-burst
                if let (Some(air_entry), Some(link)) = (self.air.get(&mac).cloned(), self.link) {
                    let zero_master = [0u8; 6];
                    let target_master = if unbind { &zero_master } else { &link.master_mac };
                    let target_rx_val = if unbind { 0u8 } else { target_rx };
                    let frame = llw_protocol::frames::bind_frame(
                        &mac,
                        target_master,
                        target_rx_val,
                        link.channel,
                        &air_entry.record.current_pwm,
                    );
                    if let Some(dongle) = self.dongle.as_mut() {
                        if let Err(e) = dongle.send_bind_burst(
                            &frame,
                            air_entry.record.channel,
                            air_entry.record.rx_type,
                        ) {
                            warn!("re-burst failed: {e}");
                        }
                    }
                }
                let op = self.pending_op.as_mut().unwrap();
                op.bursts = 2;
                op.deadline = now + BIND_DEADLINE;
            } else {
                warn!("bind op timed out for {}", mac_str(&mac));
                let op = self.pending_op.as_mut().unwrap();
                op.failed_at = Some(now);
            }
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
                    surge: Default::default(),
                    stall: Default::default(),
                    seq_index: 1,
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

/// Desktop notification for a confirmed fan stall (fires per episode —
/// stalls are rare and dangerous enough that no rate limit applies).
fn notify_stall(mac: &str) {
    let child = std::process::Command::new("notify-send")
        .arg("--urgency=critical")
        .arg("llw-daemon")
        .arg(format!("FAN STALL: {mac} commanded but not spinning"))
        .spawn();
    if let Ok(mut c) = child {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
}

/// Desktop notification for a detected fan surge (rate-limited by caller).
fn notify_surge(peak: u16, baseline: u16) {
    let child = std::process::Command::new("notify-send")
        .arg("llw-daemon")
        .arg(format!("fan surge detected: peak {peak} rpm (baseline {baseline})"))
        .spawn();
    if let Ok(mut c) = child {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
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

    /// record_bytes plus explicit fan RPMs (big-endian u16 at r[28 + i*2]).
    pub(crate) fn record_bytes_rpm(pwm: [u8; 4], fx: [u8; 4], rpms: [u16; 4]) -> [u8; 42] {
        let mut r = record_bytes(pwm, fx);
        for (i, rpm) in rpms.iter().enumerate() {
            r[28 + i * 2..30 + i * 2].copy_from_slice(&rpm.to_be_bytes());
        }
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
            std::env::temp_dir().join("llw-daemon-test-config.json"),
        );
        (sup, Instant::now())
    }

    /// Build a supervisor with an explicit config path and IPC channel.
    fn sim_with_ipc(
        cfg: Config,
        rx_script: Vec<Vec<u8>>,
        config_path: std::path::PathBuf,
    ) -> (Supervisor<FakeIo>, std::sync::mpsc::Sender<crate::ipc::IpcCmd>, Instant) {
        let (ipc_tx, ipc_rx) = std::sync::mpsc::channel();
        let sup = Supervisor::new(
            cfg,
            std::env::temp_dir(),
            Box::new(move || {
                let rx = FakeIo::default();
                for r in rx_script.clone() {
                    rx.push_read(r);
                }
                Ok(Dongle::from_parts(FakeIo::default(), Some(rx)))
            }),
            Some(ipc_rx),
            config_path,
        );
        (sup, ipc_tx, Instant::now())
    }

    /// Like `sim_with_ipc`, but with an explicit hwmon base for sensor-backed
    /// configs (fake tree in a tempdir — tests never read the real /sys).
    fn sim_with_ipc_hwmon(
        cfg: Config,
        hwmon_base: std::path::PathBuf,
        rx_script: Vec<Vec<u8>>,
        config_path: std::path::PathBuf,
    ) -> (Supervisor<FakeIo>, std::sync::mpsc::Sender<crate::ipc::IpcCmd>, Instant) {
        let (ipc_tx, ipc_rx) = std::sync::mpsc::channel();
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
            Some(ipc_rx),
            config_path,
        );
        (sup, ipc_tx, Instant::now())
    }

    /// A FakeIo shared via Arc so tests can inspect TX writes after the
    /// dongle has been moved into the supervisor.
    #[derive(Clone, Default)]
    struct SharedIo(std::sync::Arc<FakeIo>);

    impl UsbIo for SharedIo {
        fn write(&self, data: &[u8], timeout: Duration) -> llw_protocol::Result<usize> {
            self.0.write(data, timeout)
        }
        fn read(&self, buf: &mut [u8], timeout: Duration) -> llw_protocol::Result<usize> {
            self.0.read(buf, timeout)
        }
        fn read_flush(&self) {
            self.0.read_flush()
        }
    }

    /// Like `sim_with_ipc`, but the TX transport is shared out so tests can
    /// assert on the exact USB packets the supervisor sends.
    fn sim_with_ipc_shared_tx(
        cfg: Config,
        rx_script: Vec<Vec<u8>>,
        config_path: std::path::PathBuf,
    ) -> (
        Supervisor<SharedIo>,
        std::sync::mpsc::Sender<crate::ipc::IpcCmd>,
        std::sync::Arc<FakeIo>,
        Instant,
    ) {
        let (ipc_tx, ipc_rx) = std::sync::mpsc::channel();
        let tx = std::sync::Arc::new(FakeIo::default());
        let tx_in = std::sync::Arc::clone(&tx);
        let sup = Supervisor::new(
            cfg,
            std::env::temp_dir(),
            Box::new(move || {
                let rx = FakeIo::default();
                for r in rx_script.clone() {
                    rx.push_read(r);
                }
                Ok(Dongle::from_parts(
                    SharedIo(std::sync::Arc::clone(&tx_in)),
                    Some(SharedIo(std::sync::Arc::new(rx))),
                ))
            }),
            Some(ipc_rx),
            config_path,
        );
        (sup, ipc_tx, tx, Instant::now())
    }

    /// Collect the USB packets of all RF frames whose rf[1] equals `kind`,
    /// optionally restricted to frames addressed to one device MAC (rf[2..8]).
    /// Frames are 4-chunk groups; chunk 0 carries rf[0..60] at packet[4..64],
    /// so the frame kind is packet[5] and the device MAC is packet[6..12].
    fn frames_of_kind(writes: &[Vec<u8>], kind: u8, mac: Option<&[u8; 6]>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < writes.len() {
            let w = &writes[i];
            if w.len() == 64
                && w[0] == 0x10
                && w[1] == 0
                && w[5] == kind
                && mac.is_none_or(|m| &w[6..12] == m)
            {
                for j in 0..4 {
                    if let Some(p) = writes.get(i + j) {
                        out.push(p.clone());
                    }
                }
                i += 4;
            } else {
                i += 1;
            }
        }
        out
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
    fn reverted_readback_sends_a_pwm_burst() {
        // Firmware-revert recovery: a zero-readback record must trigger a
        // REVERT_BURST_REPEATS burst of the SAME pwm frame (RF-noise beating),
        // while healthy keepalives stay single-frame.
        fn pwm_frames(writes: &[Vec<u8>]) -> usize {
            writes
                .iter()
                .filter(|w| w.len() == 64 && w[0] == 0x10 && w[1] == 0 && w[4] == 0x12 && w[5] == 0x10)
                .count()
        }

        let mut cfg = test_config();
        cfg.devices[0].color = None;
        cfg.observation.poll_ms = 0;
        cfg.control.tick_ms = 0;
        cfg.control.keepalive_ms = 0; // keepalive every step

        let healthy = record_bytes([102, 102, 102, 0], [0; 4]);
        let dropped = record_bytes([0, 0, 0, 0], [0; 4]);
        let mut script = vec![getdev_resp(&[healthy]); 3];
        script.push(getdev_resp(&[dropped]));
        script.extend(vec![getdev_resp(&[healthy]); 2]);

        let dir = tempfile::tempdir().unwrap();
        let (mut sup, _ipc, tx, t0) =
            sim_with_ipc_shared_tx(cfg, script, dir.path().join("config.json"));

        let mut per_step = Vec::new();
        let mut prev = 0;
        for i in 0..6 {
            let _ = sup.step(t0 + Duration::from_secs(i + 1));
            let total = pwm_frames(&tx.written());
            per_step.push(total - prev);
            prev = total;
        }
        // Steps polling healthy records send exactly one frame; the step that
        // ingests the zeroed record bursts REVERT_BURST_REPEATS frames.
        assert!(
            per_step.contains(&REVERT_BURST_REPEATS),
            "expected one burst of {REVERT_BURST_REPEATS}, per-step sends: {per_step:?}"
        );
        assert!(
            per_step.iter().filter(|&&n| n == 1).count() >= 3,
            "healthy keepalives must stay single-frame, per-step sends: {per_step:?}"
        );
        assert!(
            !per_step.iter().any(|&n| n != 0 && n != 1 && n != REVERT_BURST_REPEATS),
            "no other send multiplicities expected: {per_step:?}"
        );
    }

    #[test]
    fn surging_dropout_window_is_counted() {
        // The audible failure mode: readback zeroes and RPM ramps toward full
        // while the window lasts. Closing the window must count ONE surge and
        // report the peak.
        let mut cfg = test_config();
        cfg.devices[0].color = None; // no RGB path — keep the script simple
        cfg.observation.poll_ms = 0;
        cfg.control.tick_ms = 0;

        // rpm stays LOW during the window; the peak lands in the tail
        // (fan inertia — the real 4Hz capture shape).
        let healthy = record_bytes_rpm([102, 102, 102, 0], [0; 4], [730, 728, 733, 0]);
        let dropped = record_bytes_rpm([0, 0, 0, 0], [0; 4], [728, 726, 731, 0]);
        let inertia = record_bytes_rpm([102, 102, 102, 0], [0; 4], [1879, 1906, 1871, 0]);
        let mut script = vec![getdev_resp(&[healthy]); 2];
        script.extend(vec![getdev_resp(&[dropped]); 2]);
        script.push(getdev_resp(&[inertia]));
        script.extend(vec![getdev_resp(&[healthy]); 10]);
        let (mut sup, t0) = sim(cfg, script);
        for i in 0..15 {
            let _ = sup.step(t0 + Duration::from_secs(i + 1));
        }
        let t = sup.telemetry();
        assert_eq!(t.total_surges, 1, "one surging episode = one surge");
        assert_eq!(t.last_surge_peak_rpm, 1906);
    }

    #[test]
    fn quiet_dropout_window_is_not_a_surge() {
        // Post-fix behavior: the flash default matches the commanded speed,
        // so a zero-readback window holds RPM near baseline. No surge.
        let mut cfg = test_config();
        cfg.devices[0].color = None;
        cfg.observation.poll_ms = 0;
        cfg.control.tick_ms = 0;

        let healthy = record_bytes_rpm([102, 102, 102, 0], [0; 4], [730, 728, 733, 0]);
        let quiet_drop = record_bytes_rpm([0, 0, 0, 0], [0; 4], [735, 871, 736, 0]);
        let mut script = vec![getdev_resp(&[healthy]); 2];
        script.extend(vec![getdev_resp(&[quiet_drop]); 2]);
        script.extend(vec![getdev_resp(&[healthy]); 10]);
        let (mut sup, t0) = sim(cfg, script);
        for i in 0..14 {
            let _ = sup.step(t0 + Duration::from_secs(i + 1));
        }
        let t = sup.telemetry();
        assert_eq!(t.total_surges, 0, "871 vs 730 baseline is wobble, not a surge");
        assert!(t.total_dropouts >= 1, "the window still counts as dropout observations");
    }

    #[test]
    fn tier1_does_not_reupload_intact_rgb() {
        // The 2026-07-17 interference incident: sustained PWM-readback zeros
        // trip Tier 1 while the onboard RGB state stays INTACT (the record's
        // effect index still matches what we uploaded). The re-acquire must
        // NOT re-upload RGB — every needless upload is a device flash write,
        // and interference storms fired 244 of them in a day.
        use llw_protocol::record::parse_device_record;

        // The index the first upload will produce for test_config's white.
        let parsed = parse_device_record(&record_bytes([102, 102, 102, 0], [0; 4]), 0).unwrap();
        let idx = rgb_assert::expected_index(&rgb_assert::static_frame(
            &parsed,
            &StaticColor { rgb: [255, 255, 255], brightness: 4 },
        ));

        let fresh = record_bytes([102, 102, 102, 0], [0; 4]); // pre-upload fx
        let healthy = record_bytes([102, 102, 102, 0], idx);
        let dropped = record_bytes([0, 0, 0, 0], idx); // PWM zeroed, RGB intact

        // acquire(fresh) → first poll uploads → healthy polls (past the 5s
        // re-upload cooldown, so cooldown can't mask a wrong re-upload) →
        // sustained zeros → Tier 1 re-acquire read → recovery polls.
        let mut script = vec![getdev_resp(&[fresh]); 2];
        script.extend(vec![getdev_resp(&[healthy]); 4]);
        script.extend(vec![getdev_resp(&[dropped]); 6]);
        script.extend(vec![getdev_resp(&[healthy]); 6]);
        let (mut sup, t0) = sim(fast_reliability_config(), script);

        let mut uploads = 0;
        let mut tier1s = 0;
        for i in 0..17 {
            let out = sup.step(t0 + Duration::from_secs(i + 1));
            uploads += out.uploaded_rgb;
            if out.tier1 {
                tier1s += 1;
            }
        }
        assert!(tier1s >= 1, "scenario must reach Tier 1");
        assert_eq!(
            uploads, 1,
            "intact RGB must not be re-uploaded after Tier 1 (flash wear)"
        );
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
            std::env::temp_dir().join("llw-daemon-test-config.json"),
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
            std::env::temp_dir().join("llw-daemon-test-config.json"),
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
            std::env::temp_dir().join("llw-daemon-test-config.json"),
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
            std::env::temp_dir().join("llw-daemon-test-config.json"),
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

    // ── Bind/Unbind IPC tests ─────────────────────────────────────────────────

    /// MAC for the device being bound (different from MAC which is already configured).
    const BIND_MAC: [u8; 6] = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];

    /// Helper: build the base config for bind tests (no color → no RGB settle window,
    /// no curves → bind creates Percent(40) slots).
    fn bind_test_config() -> Config {
        let mut cfg = Config::new();
        cfg.observation.poll_ms = 0;
        cfg.control.tick_ms = 10_000;
        cfg.control.keepalive_ms = 10_000;
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(0),
            ],
            color: None, // no RGB → no settle window
            effect: None,
        });
        cfg
    }

    /// Refusals: Bind on invalid MAC, not-visible, already-bound (Ours), foreign.
    #[test]
    fn bind_refusals() {
        let zero_master = [0u8; 6];
        let foreign_master: [u8; 6] = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let foreign_mac: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01];

        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        let rec_foreign = air_record_bytes(foreign_mac, foreign_master, [0; 4]);
        let combined = getdev_resp_multi(&[rec_ours, rec_unbound, rec_foreign]);
        let script = vec![combined; 10];

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(bind_test_config(), script, config_path);

        // Step 1: acquire
        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");
        assert_eq!(sup.air.len(), 3);

        // Bind with invalid MAC → rejected
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "not-a-mac".to_string() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(1));
        let resp = reply_rx.try_recv().unwrap();
        assert!(!resp.ok);
        assert!(resp.error.as_ref().unwrap().contains("invalid MAC"), "got: {:?}", resp.error);

        // Bind invisible device → rejected
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "11:22:33:44:55:66".to_string() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(2));
        let resp = reply_rx.try_recv().unwrap();
        assert!(!resp.ok);
        assert!(resp.error.as_ref().unwrap().contains("not visible"), "got: {:?}", resp.error);

        // Bind already-Ours device → rejected
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "02:8b:51:62:32:e1".to_string() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(3));
        let resp = reply_rx.try_recv().unwrap();
        assert!(!resp.ok);
        assert!(resp.error.as_ref().unwrap().contains("already bound"), "got: {:?}", resp.error);

        // Bind Foreign device → rejected
        let foreign_mac_str = format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            foreign_mac[0], foreign_mac[1], foreign_mac[2],
            foreign_mac[3], foreign_mac[4], foreign_mac[5]);
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: foreign_mac_str },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(4));
        let resp = reply_rx.try_recv().unwrap();
        assert!(!resp.ok);
        assert!(resp.error.as_ref().unwrap().contains("another controller"), "got: {:?}", resp.error);
    }

    /// Refusals: Unbind on not-ours.
    #[test]
    fn unbind_refusals() {
        let zero_master = [0u8; 6];
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        let combined = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let script = vec![combined; 8];

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(bind_test_config(), script, config_path);

        // Acquire
        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");

        // Unbind an Unbound device → rejected
        let bind_mac_str = "de:ad:be:ef:01:02";
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Unbind { mac: bind_mac_str.to_string() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(1));
        let resp = reply_rx.try_recv().unwrap();
        assert!(!resp.ok);
        assert!(resp.error.as_ref().unwrap().contains("not bound"), "got: {:?}", resp.error);

        // Unbind invisible MAC → rejected
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Unbind { mac: "11:22:33:44:55:66".to_string() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(2));
        let resp = reply_rx.try_recv().unwrap();
        assert!(!resp.ok);
        assert!(resp.error.as_ref().unwrap().contains("not visible"), "got: {:?}", resp.error);
    }

    /// Happy path: Bind succeeds → pending_op set → convergence detected → config saved.
    #[test]
    fn bind_success_converges_and_saves_config() {
        let zero_master = [0u8; 6];
        // MAC with rx_type=1 (set in air_record_bytes at r[13]=1), so target_rx should be 2
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]); // rx_type = 1 (r[13])
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]); // rx_type = 1

        // After bind, BIND_MAC shows bound to MASTER with rx_type=2 (target_rx)
        let rec_bound = {
            let mut r = air_record_bytes(BIND_MAC, MASTER, [0; 4]);
            r[13] = 2; // rx_type = 2 = target_rx (1 was taken by MAC)
            r
        };

        let initial = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let converged = getdev_resp_multi(&[rec_ours, rec_bound]);

        // Script: acquire + 2 polls at initial, then converged records
        let mut script = vec![initial; 3];
        script.extend(vec![converged; 6]);

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(bind_test_config(), script, config_path.clone());

        // Step 1: acquire
        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");
        assert_eq!(sup.air.len(), 2, "both devices on air");

        // Queue Bind command — will be drained in next step
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".to_string() },
            reply: reply_tx,
        }).unwrap();

        // Step 2: drains IPC → processes Bind, sends burst, sets pending_op
        // (still polls initial records — not converged yet)
        let _ = sup.step(t0 + Duration::from_secs(1));
        let resp = reply_rx.try_recv().unwrap();
        assert!(resp.ok, "bind must be accepted: {:?}", resp.error);
        assert_eq!(resp.data.as_ref().and_then(|v| v.get("state")).and_then(|v| v.as_str()), Some("started"));
        assert!(sup.pending_op.is_some(), "pending_op must be set");
        let op = sup.pending_op.as_ref().unwrap();
        assert!(!op.unbind, "must be a bind op");
        assert_eq!(op.target_rx, 2, "target_rx should be 2 (slot 1 taken by MAC)");

        // Step 3: polls initial records again (script[2]) — still not converged
        let _ = sup.step(t0 + Duration::from_secs(2));
        // Step 4: polls converged records (script[3]) — convergence detected
        let _ = sup.step(t0 + Duration::from_secs(3));

        // pending_op should be cleared after convergence
        assert!(
            sup.pending_op.is_none(),
            "pending_op must be cleared after convergence"
        );

        // Config must have BIND_MAC entry (auto-added with Percent(40) slots since no curves)
        let saved = crate::config::Config::load(&config_path).unwrap();
        assert!(
            saved.devices.iter().any(|d| d.mac == "de:ad:be:ef:01:02"),
            "bind device must be in saved config"
        );
        let dc = saved.devices.iter().find(|d| d.mac == "de:ad:be:ef:01:02").unwrap();
        assert!(
            dc.slots.iter().all(|s| *s == SlotSpeed::Percent(40)),
            "bind device must have Percent(40) slots (no curves in config)"
        );

        // DeviceRuntime must have BIND_MAC
        assert!(
            sup.devices.contains_key(&BIND_MAC),
            "runtime must have BIND_MAC after bind"
        );
    }

    /// Byte-level verification of the bind path: the burst is exactly 6×4 USB
    /// writes addressed to the record's CURRENT channel (7 — deliberately
    /// different from our link channel 2, so the two are distinguishable) and
    /// CURRENT rx, carrying a bind frame that targets OUR master at the
    /// computed target_rx with the device's current PWM passed through;
    /// save-config (3×4 writes, rx 0xFF) follows convergence — and only
    /// convergence.
    #[test]
    fn bind_burst_and_save_config_bytes() {
        let zero_master = [0u8; 6];
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]); // rx_type=1, channel=2
        let rec_unbound = {
            let mut r = air_record_bytes(BIND_MAC, zero_master, [55, 66, 77, 0]);
            r[12] = 7; // parked on channel 7 ≠ our link channel 2
            r
        };
        let rec_bound = {
            let mut r = air_record_bytes(BIND_MAC, MASTER, [55, 66, 77, 0]);
            r[13] = 2; // rx_type = target_rx
            r
        };
        // Acquisition refuses mixed-channel responses (mid-transition rule),
        // so the channel-7 device only shows up from the first poll onward.
        let acquire_resp = getdev_resp_multi(&[rec_ours]);
        let initial = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let converged = getdev_resp_multi(&[rec_ours, rec_bound]);
        let mut script = vec![acquire_resp];
        script.push(initial.clone()); // t0+1s poll — puts the ch-7 entry on air
        script.push(initial); // t0+2s poll (bind-accept step) — not yet converged
        script.extend(vec![converged; 6]);

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, tx, t0) =
            sim_with_ipc_shared_tx(bind_test_config(), script, config_path);

        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");
        let _ = sup.step(t0 + Duration::from_secs(1)); // air learns the ch-7 device

        let mark = tx.written().len();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".into() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(2));
        assert!(reply_rx.try_recv().unwrap().ok, "bind must be accepted");

        // The bind burst: 24 writes (6 frames × 4 chunks), USB-addressed to
        // the record's CURRENT channel (7, NOT our link channel 2) and
        // rx_type (1). Heartbeat frames (rf[1]=0x14) and the configured
        // device's PWM frame (0x10 but a different device MAC) on the same
        // transport are filtered out.
        let all = tx.written();
        let bind_writes = frames_of_kind(&all[mark..], 0x10, Some(&BIND_MAC));
        assert_eq!(bind_writes.len(), 24, "bind burst must be exactly 6×4=24 USB writes");
        for (i, w) in bind_writes.iter().enumerate() {
            assert_eq!(w[2], 7, "packet[{i}][2] must be the record's CURRENT channel (7), not the link channel");
            assert_eq!(w[3], 1, "packet[{i}][3] must be the record's CURRENT rx_type");
        }
        // Pin the RF payload via chunk 0 of the first frame (rf[n] = packet[4+n]).
        let first = &bind_writes[0];
        assert_eq!(first[4], 0x12, "rf[0] = RF_SELECT");
        assert_eq!(first[5], 0x10, "rf[1] = RF_PWM_CMD (bind reuses the layout)");
        assert_eq!(&first[6..12], &BIND_MAC, "rf[2..8] = device MAC");
        assert_eq!(&first[12..18], &MASTER, "rf[8..14] = OUR master (bind target)");
        assert_eq!(first[18], 2, "rf[14] = target_rx");
        assert_eq!(first[19], 2, "rf[15] = LINK channel (2) — distinct from the record's channel (7)");
        assert_eq!(first[20], 2, "rf[16] = target_rx (seq-byte reuse)");
        assert_eq!(&first[21..25], &[55, 66, 77, 0], "rf[17..21] = current PWM passthrough");
        assert!(
            frames_of_kind(&all[mark..], 0x15, None).is_empty(),
            "no save-config before convergence"
        );

        // Convergence step: record flips to our-master + target_rx.
        let mark = tx.written().len();
        let _ = sup.step(t0 + Duration::from_secs(3));
        assert!(sup.pending_op.is_none(), "must converge");
        let all = tx.written();
        let save_writes = frames_of_kind(&all[mark..], 0x15, None);
        assert_eq!(save_writes.len(), 12, "save-config must be exactly 3×4=12 USB writes");
        for (i, w) in save_writes.iter().enumerate() {
            assert_eq!(w[2], 2, "save packet[{i}][2] must be the master channel");
            assert_eq!(w[3], 0xFF, "save packet[{i}][3] must be 0xFF");
        }
    }

    /// A second bind/unbind while one op is pending must be refused.
    #[test]
    fn bind_concurrent_op_refusal() {
        let zero_master = [0u8; 6];
        let second_mac: [u8; 6] = [0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x01];
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound_a = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        let rec_unbound_b = air_record_bytes(second_mac, zero_master, [0; 4]);
        let combined = getdev_resp_multi(&[rec_ours, rec_unbound_a, rec_unbound_b]);
        let script = vec![combined; 10];

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(bind_test_config(), script, config_path);

        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");

        // Queue three commands: the first bind is accepted; the second bind
        // (different device) and an unbind must both be refused while the
        // first op is pending. drain_ipc processes all three in one step.
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".into() },
            reply: reply_tx,
        }).unwrap();
        let (reply_tx2, reply_rx2) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "ca:fe:ba:be:00:01".into() },
            reply: reply_tx2,
        }).unwrap();
        let (reply_tx3, reply_rx3) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Unbind { mac: "02:8b:51:62:32:e1".into() },
            reply: reply_tx3,
        }).unwrap();

        let _ = sup.step(t0 + Duration::from_secs(1));
        let resp = reply_rx.try_recv().unwrap();
        assert!(resp.ok, "first bind must be accepted: {:?}", resp.error);
        let resp2 = reply_rx2.try_recv().unwrap();
        assert!(!resp2.ok, "second bind must be refused while an op is pending");
        assert!(
            resp2.error.as_ref().unwrap().contains("already in progress"),
            "got: {:?}", resp2.error
        );
        let resp3 = reply_rx3.try_recv().unwrap();
        assert!(!resp3.ok, "unbind must be refused while an op is pending");
        assert!(
            resp3.error.as_ref().unwrap().contains("already in progress"),
            "got: {:?}", resp3.error
        );
    }

    /// Happy path: Unbind succeeds → pending_op set → convergence detected → config saved.
    #[test]
    fn unbind_success_converges_and_removes_from_config() {
        // MAC is pre-configured (Ours) — we will unbind it.
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);

        // After unbind, MAC shows with zero master (Unbound)
        let zero_master = [0u8; 6];
        let rec_unbound = air_record_bytes(MAC, zero_master, [0; 4]);

        let initial = getdev_resp_multi(&[rec_ours]);
        let converged = getdev_resp_multi(&[rec_unbound]);

        let mut script = vec![initial; 3];
        script.extend(vec![converged; 6]);

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(bind_test_config(), script, config_path.clone());

        // Acquire
        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");
        assert_eq!(sup.air.get(&MAC).unwrap().bond, Bond::Ours);

        // Queue Unbind
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Unbind { mac: "02:8b:51:62:32:e1".to_string() },
            reply: reply_tx,
        }).unwrap();

        // Step: process Unbind
        let _ = sup.step(t0 + Duration::from_secs(1));
        let resp = reply_rx.try_recv().unwrap();
        assert!(resp.ok, "unbind must be accepted: {:?}", resp.error);
        assert!(sup.pending_op.is_some(), "pending_op must be set");

        // Step 3: polls initial records (script[2]) — still not converged
        let _ = sup.step(t0 + Duration::from_secs(2));
        // Step 4: polls converged records (script[3]) — convergence detected
        let _ = sup.step(t0 + Duration::from_secs(3));
        assert!(sup.pending_op.is_none(), "pending_op must be cleared after convergence");

        // Config must have MAC removed
        let saved = crate::config::Config::load(&config_path).unwrap();
        assert!(
            !saved.devices.iter().any(|d| d.mac == "02:8b:51:62:32:e1"),
            "unbound device must be removed from saved config"
        );

        // DeviceRuntime must be gone
        assert!(
            !sup.devices.contains_key(&MAC),
            "runtime must not have MAC after unbind"
        );
    }

    /// Bind times out (no convergence) → op marked failed, then auto-cleared after 30s.
    #[test]
    fn bind_timeout_marks_failed_then_clears() {
        let zero_master = [0u8; 6];
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]);

        // BIND_MAC never converges — stays unbound forever
        let combined = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let script = vec![combined; 30];

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(bind_test_config(), script, config_path.clone());

        // Acquire
        let out = sup.step(t0);
        assert!(out.acquired);

        // Queue Bind
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".to_string() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(1));
        let resp = reply_rx.try_recv().unwrap();
        assert!(resp.ok, "bind must be accepted initially");

        // Step past BIND_DEADLINE (5s) + re-burst + second deadline → must fail
        let _ = sup.step(t0 + Duration::from_secs(7));  // past first deadline
        let _ = sup.step(t0 + Duration::from_secs(13)); // past second deadline → failed
        assert!(
            sup.pending_op.as_ref().map(|o| o.failed_at.is_some()).unwrap_or(false),
            "op must be marked failed after two deadlines"
        );

        // A failed bind must leave NO trace: no runtime, no in-memory config
        // entry, and nothing saved to disk.
        assert!(
            !sup.devices.contains_key(&BIND_MAC),
            "failed bind must not create a DeviceRuntime"
        );
        assert!(
            !sup.cfg.devices.iter().any(|d| d.mac == "de:ad:be:ef:01:02"),
            "failed bind must not add an in-memory config entry"
        );
        let saved = crate::config::Config::load(&config_path).unwrap();
        assert!(
            !saved.devices.iter().any(|d| d.mac == "de:ad:be:ef:01:02"),
            "failed bind must not persist a config entry"
        );

        // BIND_FAIL_CLEAR after 30s: op is auto-cleared
        let _ = sup.step(t0 + Duration::from_secs(44)); // 13 + 31 > 30s clear window
        assert!(
            sup.pending_op.is_none(),
            "failed op must be auto-cleared after 30s"
        );
    }

    /// Status IPC response includes the pending op field.
    #[test]
    fn status_includes_pending_op() {
        let zero_master = [0u8; 6];
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        let combined = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let script = vec![combined; 10];

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(bind_test_config(), script, config_path);

        // Acquire
        let _ = sup.step(t0);

        // Queue Bind
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".to_string() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(1));
        let _ = reply_rx.try_recv().unwrap(); // consume bind reply

        // Now send Status and check it shows pending
        let (status_reply_tx, status_reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Status,
            reply: status_reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(2));
        let status_resp = status_reply_rx.try_recv().unwrap();
        assert!(status_resp.ok);
        let data = status_resp.data.unwrap();
        let pending = data.get("pending").unwrap();
        assert!(!pending.is_null(), "pending must not be null while bind is in progress");
        assert_eq!(pending.get("op").and_then(|v| v.as_str()), Some("bind"));
        assert_eq!(pending.get("state").and_then(|v| v.as_str()), Some("converging"));
        assert_eq!(pending.get("mac").and_then(|v| v.as_str()), Some("de:ad:be:ef:01:02"));
    }

    /// Status must expose each configured curve's smoothed sensor reading —
    /// the same EMA value the fan controller uses — and null for a curve
    /// whose sensor never resolved.
    #[test]
    fn status_includes_curve_temps() {
        use crate::config::{Curve, SensorSpec};

        // Fake hwmon tree: k10temp temp1 = 41.3°C. The "gpu" curve's chip
        // does not exist → its sensor never resolves.
        let dir = tempfile::tempdir().unwrap();
        let hwmon0 = dir.path().join("hwmon0");
        std::fs::create_dir_all(&hwmon0).unwrap();
        std::fs::write(hwmon0.join("name"), "k10temp\n").unwrap();
        std::fs::write(hwmon0.join("temp1_input"), "41300\n").unwrap();

        let mut cfg = Config::new();
        cfg.curves.push(Curve {
            name: "cpu".into(),
            sensor: SensorSpec { hwmon_name: "k10temp".into(), input: "temp1_input".into() },
            points: vec![(29.0, 30.0), (52.0, 34.0)],
        });
        cfg.curves.push(Curve {
            name: "gpu".into(),
            sensor: SensorSpec { hwmon_name: "missing".into(), input: "temp1_input".into() },
            points: vec![(30.0, 20.0), (70.0, 100.0)],
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
        cfg.control.keepalive_ms = 1_000_000;
        cfg.observation.poll_ms = 0; // poll every step

        let rec = record_bytes([0; 4], [0; 4]);
        let script = vec![getdev_resp(&[rec]); 10];
        let tmp = tempfile::tempdir().unwrap();
        let (mut sup, ipc_tx, t0) = sim_with_ipc_hwmon(
            cfg,
            dir.path().to_path_buf(),
            script,
            tmp.path().join("config.json"),
        );

        // Acquire, then one fan-tick step so the cpu curve reads its sensor.
        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");
        let _ = sup.step(t0 + Duration::from_secs(1));

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Status,
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(2));
        let resp = reply_rx.try_recv().unwrap();
        assert!(resp.ok, "{:?}", resp.error);
        let data = resp.data.unwrap();
        let curves = data.get("curves").unwrap().as_array().unwrap();
        assert_eq!(curves.len(), 2, "one entry per configured curve, in config order");
        assert_eq!(curves[0]["name"], "cpu");
        let c = curves[0]["sensor_c"].as_f64().unwrap();
        assert!((c - 41.3).abs() < 0.01, "cpu sensor_c must be the EMA'd 41.3°C, got {c}");
        assert_eq!(curves[1]["name"], "gpu");
        assert!(curves[1]["sensor_c"].is_null(), "unresolvable sensor must be null");
    }

    /// ListSensors over IPC enumerates the supervisor's hwmon base, and each
    /// emitted spec is verbatim-usable as a Curve's `sensor` (it resolves
    /// against the same tree to the same reading).
    #[test]
    fn list_sensors_ipc_enumerates_fake_tree() {
        let dir = tempfile::tempdir().unwrap();
        let hwmon0 = dir.path().join("hwmon0");
        std::fs::create_dir_all(&hwmon0).unwrap();
        std::fs::write(hwmon0.join("name"), "k10temp\n").unwrap();
        std::fs::write(hwmon0.join("temp1_input"), "41300\n").unwrap();
        std::fs::write(hwmon0.join("temp1_label"), "Tctl\n").unwrap();

        let rec = record_bytes([0; 4], [0; 4]);
        let script = vec![getdev_resp(&[rec]); 4];
        let tmp = tempfile::tempdir().unwrap();
        let (mut sup, ipc_tx, t0) = sim_with_ipc_hwmon(
            bind_test_config(),
            dir.path().to_path_buf(),
            script,
            tmp.path().join("config.json"),
        );
        let _ = sup.step(t0);

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::ListSensors,
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(1));
        let resp = reply_rx.try_recv().unwrap();
        assert!(resp.ok, "{:?}", resp.error);
        let data = resp.data.unwrap();
        let sensors = data["sensors"].as_array().unwrap();
        assert_eq!(sensors.len(), 1);
        assert_eq!(sensors[0]["chip"], "k10temp");
        assert_eq!(sensors[0]["label"], "Tctl");
        assert!((sensors[0]["current_c"].as_f64().unwrap() - 41.3).abs() < 0.01);
        // Verbatim usability: deserialize the emitted spec as a config
        // SensorSpec and resolve it against the same tree.
        let spec: crate::config::SensorSpec =
            serde_json::from_value(sensors[0]["spec"].clone()).unwrap();
        let sensor = crate::sensors::resolve(dir.path(), &spec).expect("spec must resolve");
        assert!((sensor.read_c().unwrap() - 41.3).abs() < 0.001);
    }

    /// When the config has at least one curve, a successful bind auto-adds the
    /// device with all four slots bound to the FIRST curve, not Percent(40).
    #[test]
    fn bind_auto_entry_uses_first_curve() {
        let zero_master = [0u8; 6];
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        let rec_bound = {
            let mut r = air_record_bytes(BIND_MAC, MASTER, [0; 4]);
            r[13] = 2; // rx_type = target_rx
            r
        };
        let initial = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let converged = getdev_resp_multi(&[rec_ours, rec_bound]);
        let mut script = vec![initial; 3];
        script.extend(vec![converged; 6]);

        let mut cfg = bind_test_config();
        cfg.curves.push(crate::config::Curve {
            name: "quiet".into(),
            sensor: crate::config::SensorSpec {
                hwmon_name: "k10temp".into(),
                input: "temp1_input".into(),
            },
            points: vec![(30.0, 20.0), (70.0, 100.0)],
        });

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(cfg, script, config_path.clone());

        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".into() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(1));
        assert!(reply_rx.try_recv().unwrap().ok, "bind must be accepted");
        let _ = sup.step(t0 + Duration::from_secs(2));
        let _ = sup.step(t0 + Duration::from_secs(3));
        assert!(sup.pending_op.is_none(), "must converge");

        let saved = crate::config::Config::load(&config_path).unwrap();
        let dc = saved.devices.iter().find(|d| d.mac == "de:ad:be:ef:01:02").unwrap();
        assert!(
            dc.slots.iter().all(|s| *s == SlotSpeed::Curve("quiet".into())),
            "auto-entry slots must use the first curve, got {:?}", dc.slots
        );
    }

    /// A new Bind arriving during the post-save-config RF settle window must
    /// be refused with the settling error.
    #[test]
    fn bind_refused_during_settle_window() {
        let zero_master = [0u8; 6];
        let second_mac: [u8; 6] = [0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x01];
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound_a = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        let rec_unbound_b = air_record_bytes(second_mac, zero_master, [0; 4]);
        let rec_bound_a = {
            let mut r = air_record_bytes(BIND_MAC, MASTER, [0; 4]);
            r[13] = 2;
            r
        };
        let initial = getdev_resp_multi(&[rec_ours, rec_unbound_a, rec_unbound_b]);
        let converged = getdev_resp_multi(&[rec_ours, rec_bound_a, rec_unbound_b]);
        let mut script = vec![initial; 2];
        script.extend(vec![converged; 8]);

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(bind_test_config(), script, config_path);

        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");

        // First bind: accepted at t0+1s, converges at t0+2s → save-config +
        // settle window until t0+5s.
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".into() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(1));
        assert!(reply_rx.try_recv().unwrap().ok, "first bind must be accepted");
        let _ = sup.step(t0 + Duration::from_secs(2));
        assert!(sup.pending_op.is_none(), "first op must converge");
        assert!(sup.rf_settle_until.is_some(), "settle window must be open");

        // Second bind (a different, perfectly bindable device) inside the
        // settle window → refused with the settling error, no pending op.
        let (reply_tx2, reply_rx2) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "ca:fe:ba:be:00:01".into() },
            reply: reply_tx2,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(3)); // inside settle (t0+2s..t0+5s)
        let resp = reply_rx2.try_recv().unwrap();
        assert!(!resp.ok, "bind during settle must be refused");
        assert!(
            resp.error.as_ref().unwrap().contains("settling"),
            "got: {:?}", resp.error
        );
        assert!(sup.pending_op.is_none(), "refused bind must not create an op");
    }

    /// RF-silence invariant: the forced convergence polls during a settle
    /// window must NOT trigger RGB uploads. A second configured device whose
    /// first RGB assertion comes due mid-settle (record first seen then) must
    /// stay silent until the window closes.
    ///
    /// (Same-device drift can never re-upload inside a window: the 5s
    /// re-upload cooldown outlasts the 3s settle. A never-asserted device is
    /// the only path that makes this test non-vacuous.)
    #[test]
    fn settle_with_pending_op_suppresses_rgb_tick() {
        let zero_master = [0u8; 6];
        let second_cfg_mac: [u8; 6] = [0x02, 0x8b, 0x51, 0x62, 0x32, 0xe2];
        let mut cfg = bind_test_config();
        // Both configured devices carry a static color so rgb_tick has work.
        cfg.devices[0].color = Some(StaticColor { rgb: [255, 0, 0], brightness: 4 });
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e2".into(),
            name: None,
            slots: [
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(0),
            ],
            color: Some(StaticColor { rgb: [0, 255, 0], brightness: 4 }),
            effect: None,
        });

        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_ours_b = air_record_bytes(second_cfg_mac, MASTER, [0; 4]);
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        // Device B is absent until t0+2s so its first (would-be) upload comes
        // due during the settle window opened by device A's upload at t0+1s.
        let without_b = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let with_b = getdev_resp_multi(&[rec_ours, rec_ours_b, rec_unbound]);
        let mut script = vec![without_b; 2];
        script.extend(vec![with_b; 8]);

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(cfg, script, config_path);

        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");

        // t0+1s: Bind accepted at step start (settle not yet open); the poll's
        // rgb_tick then uploads for device A and opens the settle window —
        // leaving a pending op inside an active settle.
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".into() },
            reply: reply_tx,
        }).unwrap();
        let out = sup.step(t0 + Duration::from_secs(1));
        assert!(reply_rx.try_recv().unwrap().ok, "bind must be accepted");
        assert_eq!(out.uploaded_rgb, 1, "device A's color must upload at t0+1s");
        assert!(sup.rf_settle_until.is_some(), "settle window must be open");
        assert!(sup.pending_op.is_some(), "op must still be pending");

        // t0+2s (inside settle): the pending op forces the poll, which first
        // ingests device B's record — B has never been asserted (expected_fx
        // None, no cooldown), so only the settle gate stands between it and
        // an upload. It must hold.
        let out = sup.step(t0 + Duration::from_secs(2));
        assert!(out.polled, "pending op must force the poll during settle");
        assert!(
            sup.devices.get(&second_cfg_mac).unwrap().last_record.is_some(),
            "device B's record must have been ingested"
        );
        assert_eq!(out.uploaded_rgb, 0, "no RGB upload during the settle window");
    }

    /// Review carry-forward sim: re-bind dedup.
    ///
    /// A device that is already in the config with custom slots (Percent(55))
    /// but is currently Unbound on the air (e.g. after a power-cycle while it
    /// wasn't the active controller) can be re-bound via Bind. On convergence:
    /// - exactly ONE config entry must exist for the MAC (no duplicate added)
    /// - the original Percent(55) slots must be preserved (not reset to defaults)
    #[test]
    fn rebind_dedup_preserves_existing_slots() {
        let zero_master = [0u8; 6];
        // BIND_MAC is already in config with custom Percent(55) slots.
        let mut cfg = bind_test_config();
        cfg.devices.push(DeviceConfig {
            mac: "de:ad:be:ef:01:02".into(),
            name: None,
            slots: [
                SlotSpeed::Percent(55),
                SlotSpeed::Percent(55),
                SlotSpeed::Percent(55),
                SlotSpeed::Percent(55),
            ],
            color: None,
            effect: None,
        });

        // Air: MAC (Ours) + BIND_MAC (Unbound — lost bond after power-cycle)
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        // After bind, BIND_MAC shows bound to MASTER with rx_type=2
        let rec_bound = {
            let mut r = air_record_bytes(BIND_MAC, MASTER, [0; 4]);
            r[13] = 2; // rx_type = 2 = target_rx
            r
        };

        let initial = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let converged = getdev_resp_multi(&[rec_ours, rec_bound]);
        let mut script = vec![initial; 3];
        script.extend(vec![converged; 6]);

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, t0) = sim_with_ipc(cfg, script, config_path.clone());

        // Acquire
        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");
        assert_eq!(sup.air.len(), 2, "both devices on air");

        // Queue Bind on the already-configured-but-Unbound device
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".to_string() },
            reply: reply_tx,
        }).unwrap();

        // Step: drains IPC → processes Bind → accepted (Unbound → can be bound)
        let _ = sup.step(t0 + Duration::from_secs(1));
        let resp = reply_rx.try_recv().unwrap();
        assert!(resp.ok, "re-bind must be accepted for Unbound device: {:?}", resp.error);
        assert!(sup.pending_op.is_some(), "pending_op must be set");

        // Step: still initial records (not converged yet)
        let _ = sup.step(t0 + Duration::from_secs(2));
        // Step: converged records → convergence detected
        let _ = sup.step(t0 + Duration::from_secs(3));
        assert!(sup.pending_op.is_none(), "pending_op must be cleared after convergence");

        // Verify config: exactly ONE entry for BIND_MAC
        let saved = crate::config::Config::load(&config_path).unwrap();
        let entries: Vec<_> = saved.devices.iter()
            .filter(|d| d.mac == "de:ad:be:ef:01:02")
            .collect();
        assert_eq!(entries.len(), 1, "must be exactly ONE config entry for the MAC, got {}", entries.len());

        // Original Percent(55) slots must be preserved (not reset to defaults)
        let dc = entries[0];
        assert!(
            dc.slots.iter().all(|s| *s == SlotSpeed::Percent(55)),
            "re-bind must preserve original Percent(55) slots, got {:?}", dc.slots
        );

        // DeviceRuntime must be present
        assert!(
            sup.devices.contains_key(&BIND_MAC),
            "runtime must have BIND_MAC after re-bind"
        );
    }

    /// CRITICAL invariant: the deadline re-burst must be gated on the bond
    /// still matching the op's precondition. If another controller claims the
    /// device mid-op (Unbound → Foreign), the op fails immediately and NO
    /// second burst is transmitted at the foreign-bound device.
    #[test]
    fn bind_reburst_aborts_when_bond_flips_foreign() {
        let zero_master = [0u8; 6];
        let foreign_master: [u8; 6] = [0xBA, 0xDF, 0x00, 0xD5, 0x00, 0x01];
        let rec_ours = air_record_bytes(MAC, MASTER, [0; 4]);
        let rec_unbound = air_record_bytes(BIND_MAC, zero_master, [0; 4]);
        let rec_foreign = air_record_bytes(BIND_MAC, foreign_master, [0; 4]);

        let initial = getdev_resp_multi(&[rec_ours, rec_unbound]);
        let stolen = getdev_resp_multi(&[rec_ours, rec_foreign]);
        let mut script = vec![initial; 2]; // acquire + accept-step poll
        script.extend(vec![stolen; 8]); // another controller claimed it

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.json");
        let (mut sup, ipc_tx, tx, t0) =
            sim_with_ipc_shared_tx(bind_test_config(), script, config_path);

        let out = sup.step(t0);
        assert!(out.acquired, "must acquire");

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        ipc_tx.send(crate::ipc::IpcCmd {
            req: crate::ipc::Request::Bind { mac: "de:ad:be:ef:01:02".into() },
            reply: reply_tx,
        }).unwrap();
        let _ = sup.step(t0 + Duration::from_secs(1));
        assert!(reply_rx.try_recv().unwrap().ok, "bind must be accepted");

        // t0+2s: poll reveals the device has gone Foreign (not converged).
        let _ = sup.step(t0 + Duration::from_secs(2));
        assert_eq!(sup.air.get(&BIND_MAC).unwrap().bond, Bond::Foreign);
        let mark = tx.written().len();

        // t0+6s: past the first deadline — instead of re-bursting at a
        // foreign-bound device, the op must fail on the spot.
        let _ = sup.step(t0 + Duration::from_secs(6));
        let op = sup.pending_op.as_ref().expect("op must still be visible");
        assert!(op.failed_at.is_some(), "op must be marked failed, not re-burst");
        assert_eq!(op.bursts, 1, "burst count must not advance");
        let all = tx.written();
        assert!(
            frames_of_kind(&all[mark..], 0x10, Some(&BIND_MAC)).is_empty(),
            "NO bind frames may be transmitted at a foreign-bound device"
        );
    }
}
