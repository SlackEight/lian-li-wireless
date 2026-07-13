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
