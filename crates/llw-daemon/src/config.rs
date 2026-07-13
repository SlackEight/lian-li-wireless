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
    /// One entry per fan slot. RGB-only devices set all four slots to 0.
    pub slots: [SlotSpeed; 4],
    /// Static color asserted (and drift-restored) by the daemon. None = leave alone.
    /// M3 note: richer effects will arrive as a separate optional field; if both
    /// are present, the effect takes precedence over this static color.
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

    #[allow(dead_code)]
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
}
