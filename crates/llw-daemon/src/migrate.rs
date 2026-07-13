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
