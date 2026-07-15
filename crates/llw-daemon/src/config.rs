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
    #[serde(default)]
    pub observation: ObservationConfig,
    /// Saved effect presets. Pass-through data for the UI: the daemon stores
    /// and round-trips these via GetConfig/SetConfig but never interprets,
    /// validates, or applies them.
    #[serde(default)]
    pub presets: Vec<Preset>,
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
    /// One entry per fan slot. RGB-only devices set all four slots to 0.
    pub slots: [SlotSpeed; 4],
    /// Static color asserted (and drift-restored) by the daemon. None = leave alone.
    /// If both `effect` and `color` are present, `effect` takes precedence.
    #[serde(default)]
    pub color: Option<StaticColor>,
    /// Animated effect spec (M3). When present, overrides `color`. Speed must be
    /// 1..=5 (0 is invalid), brightness ≤4, palette ≤8 entries.
    #[serde(default)]
    pub effect: Option<llw_effects::EffectSpec>,
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

/// A named effect the UI offers as a one-click starting point (M4c).
/// Storage only — see the `presets` field on [`Config`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub effect: llw_effects::EffectSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlConfig {
    /// Fan control tick in ms.
    pub tick_ms: u64,
    pub hysteresis_temp: f32,
    pub hysteresis_pwm: u8,
    /// PWM keepalive interval in ms (firmware reverts without traffic).
    pub keepalive_ms: u64,
    /// Fan % commanded when a curve's sensor has been unreadable for over a
    /// minute — or immediately if it has never produced a reading.
    #[serde(default = "default_sensor_failsafe")]
    pub sensor_failsafe_percent: u8,
}

fn default_sensor_failsafe() -> u8 {
    50
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            tick_ms: 1000,
            hysteresis_temp: 1.0,
            hysteresis_pwm: 5,
            keepalive_ms: 1000,
            sensor_failsafe_percent: 50,
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

/// How raw GetDev readbacks become dropout observations (experiment-tuned:
/// single-poll blips are normal on a healthy channel and self-heal via the
/// 1s keepalive; only persistent readback loss is evidence of link trouble).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationConfig {
    /// A dropout observation is reported for every poll at or beyond this
    /// many CONSECUTIVE all-zero-readback polls while PWM is commanded.
    #[serde(default = "default_consecutive_polls")]
    pub consecutive_polls: u32,
    /// GetDev poll interval in ms.
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
}

fn default_consecutive_polls() -> u32 {
    2
}

fn default_poll_ms() -> u64 {
    500
}

impl Default for ObservationConfig {
    fn default() -> Self {
        Self { consecutive_polls: 2, poll_ms: 500 }
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
    /// The correct constructor for any instance that will be serialized —
    /// the derived `Default` leaves schema_version at 0, which `load` rejects.
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
                match slot {
                    SlotSpeed::Curve(name) => {
                        if !self.curves.iter().any(|c| &c.name == name) {
                            bail!("device {} references unknown curve {:?}", dev.mac, name);
                        }
                    }
                    SlotSpeed::Percent(pct) if *pct > 100 => {
                        bail!("device {} slot percent {} out of range 0-100", dev.mac, pct);
                    }
                    SlotSpeed::Percent(_) => {}
                }
            }
            if let Some(c) = &dev.color {
                if c.brightness > 4 {
                    bail!("device {} brightness {} out of range 0-4", dev.mac, c.brightness);
                }
            }
            if let Some(eff) = &dev.effect {
                validate_effect(eff)
                    .with_context(|| format!("device {} effect", dev.mac))?;
            }
        }
        Ok(())
    }
}

/// Validate an [`llw_effects::EffectSpec`] in isolation (speed, brightness, palette size).
/// Called from [`Config::validate`] and from the IPC `SetEffect` handler so the rules
/// can never drift between config-load and runtime.
pub fn validate_effect(spec: &llw_effects::EffectSpec) -> Result<()> {
    if spec.speed == 0 || spec.speed > 5 {
        bail!("effect speed {} out of range 1-5", spec.speed);
    }
    if spec.brightness > 4 {
        bail!("effect brightness {} out of range 0-4", spec.brightness);
    }
    if spec.colors.len() > 8 {
        bail!("effect palette has {} entries (max 8)", spec.colors.len());
    }
    Ok(())
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
            effect: None,
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
        assert_eq!(loaded.observation.consecutive_polls, 2);
        assert_eq!(loaded.control.sensor_failsafe_percent, 50);
    }

    #[test]
    fn pre_m2b_config_files_still_load() {
        // shape written by the M2a importer BEFORE observation/failsafe existed
        let old = r#"{
            "schema_version": 1,
            "curves": [],
            "devices": [],
            "control": {"tick_ms": 200, "hysteresis_temp": 1.0, "hysteresis_pwm": 5, "keepalive_ms": 1000}
        }"#;
        let cfg: Config = serde_json::from_str(old).unwrap();
        assert_eq!(cfg.control.sensor_failsafe_percent, 50); // NOT 0
        assert_eq!(cfg.observation.consecutive_polls, 2);
        // partial observation object also tolerated
        let partial = r#"{"schema_version": 1, "observation": {"poll_ms": 100}}"#;
        let cfg: Config = serde_json::from_str(partial).unwrap();
        assert_eq!(cfg.observation.poll_ms, 100);
        assert_eq!(cfg.observation.consecutive_polls, 2);
    }

    #[test]
    fn pre_m4c_config_files_have_no_presets() {
        // shape written before the presets field existed (M4c)
        let old = r#"{
            "schema_version": 1,
            "curves": [],
            "devices": []
        }"#;
        let cfg: Config = serde_json::from_str(old).unwrap();
        assert!(cfg.presets.is_empty());
    }

    #[test]
    fn presets_roundtrip() {
        use llw_effects::{Direction, EffectKind, EffectSpec};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = Config::new();
        // full spec, palette included
        cfg.presets.push(Preset {
            name: "ocean".into(),
            effect: EffectSpec {
                kind: EffectKind::Meteor,
                colors: vec![[0, 64, 255], [0, 255, 200], [255, 255, 255]],
                speed: 2,
                direction: Direction::Reverse,
                brightness: 3,
            },
        });
        // minimal wire shape — everything but `kind` filled by EffectSpec serde defaults
        let minimal: Preset =
            serde_json::from_str(r#"{"name": "plain", "effect": {"kind": "breathing"}}"#).unwrap();
        assert_eq!(minimal.effect.speed, 3);
        assert_eq!(minimal.effect.brightness, 4);
        assert!(minimal.effect.colors.is_empty());
        cfg.presets.push(minimal);
        cfg.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.presets, cfg.presets);
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
    fn out_of_range_percent_rejected() {
        // serde accepts any u8; validate() must catch >100
        let s: SlotSpeed = serde_json::from_str("150").unwrap();
        assert_eq!(s, SlotSpeed::Percent(150));
        let mut cfg = sample();
        cfg.devices[0].slots[0] = SlotSpeed::Percent(150);
        assert!(cfg.validate().is_err());
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

    #[test]
    fn effect_roundtrip() {
        use llw_effects::{Direction, EffectKind, EffectSpec};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = Config::new();
        cfg.devices.push(DeviceConfig {
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
        cfg.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        let eff = loaded.devices[0].effect.as_ref().unwrap();
        assert_eq!(eff.kind, EffectKind::Ripple);
        assert_eq!(eff.speed, 3);
        assert_eq!(eff.brightness, 4);
        assert_eq!(eff.colors, vec![[0u8, 0, 255], [136, 0, 255]]);
    }

    #[test]
    fn effect_validation_rejects_speed_zero() {
        use llw_effects::{Direction, EffectKind, EffectSpec};
        let mut cfg = Config::new();
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0)],
            color: None,
            effect: Some(EffectSpec {
                kind: EffectKind::Ripple,
                colors: vec![],
                speed: 0, // invalid
                direction: Direction::Forward,
                brightness: 4,
            }),
        });
        assert!(cfg.validate().is_err(), "speed 0 must be rejected");
    }

    #[test]
    fn effect_validation_rejects_speed_out_of_range() {
        use llw_effects::{Direction, EffectKind, EffectSpec};
        let mut cfg = Config::new();
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0)],
            color: None,
            effect: Some(EffectSpec {
                kind: EffectKind::Ripple,
                colors: vec![],
                speed: 6, // invalid
                direction: Direction::Forward,
                brightness: 4,
            }),
        });
        assert!(cfg.validate().is_err(), "speed 6 must be rejected");
    }

    #[test]
    fn effect_validation_rejects_brightness_overflow() {
        use llw_effects::{Direction, EffectKind, EffectSpec};
        let mut cfg = Config::new();
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0)],
            color: None,
            effect: Some(EffectSpec {
                kind: EffectKind::Ripple,
                colors: vec![],
                speed: 3,
                direction: Direction::Forward,
                brightness: 5, // invalid
            }),
        });
        assert!(cfg.validate().is_err(), "brightness 5 must be rejected");
    }

    #[test]
    fn effect_validation_rejects_too_many_colors() {
        use llw_effects::{Direction, EffectKind, EffectSpec};
        let mut cfg = Config::new();
        cfg.devices.push(DeviceConfig {
            mac: "02:8b:51:62:32:e1".into(),
            name: None,
            slots: [SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0), SlotSpeed::Percent(0)],
            color: None,
            effect: Some(EffectSpec {
                kind: EffectKind::Ripple,
                colors: vec![[0, 0, 0]; 9], // max is 8
                speed: 3,
                direction: Direction::Forward,
                brightness: 4,
            }),
        });
        assert!(cfg.validate().is_err(), "palette > 8 entries must be rejected");
    }
}
