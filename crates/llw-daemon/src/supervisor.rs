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
use llw_protocol::dongle::Dongle;
use llw_protocol::frames::{apply_pwm_constraints, master_clock_frame, pwm_frame};
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
}
