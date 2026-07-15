//! Native hwmon temperature reading (replaces lianli-daemon's shell commands).

use crate::config::SensorSpec;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub struct HwmonSensor {
    input_path: PathBuf,
}

/// One enumerable hwmon temperature channel (a `ListSensors` reply item).
/// `spec` is usable verbatim as a config `Curve`'s `sensor` field.
#[derive(Debug, Serialize, Deserialize)]
pub struct SensorInfo {
    /// Chip name (`hwmon*/name`), e.g. "k10temp".
    pub chip: String,
    /// Channel label (`temp*_label` when present, else the input stem, e.g. "temp1").
    pub label: String,
    pub spec: SensorSpec,
    /// Best-effort current reading in °C; None on read/parse failure.
    pub current_c: Option<f32>,
}

/// Enumerate every `temp*_input` channel under a hwmon tree root
/// (production: /sys/class/hwmon). Chips without a readable `name` file are
/// skipped — a [`SensorSpec`] cannot address them. Order is deterministic:
/// hwmon index, then temp channel index.
pub fn enumerate(base: &Path) -> Result<Vec<SensorInfo>> {
    let entries = std::fs::read_dir(base)
        .with_context(|| format!("reading hwmon dir {}", base.display()))?;
    let mut chips: Vec<(u32, PathBuf, String)> = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(name) = std::fs::read_to_string(dir.join("name")) else { continue };
        let index = dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("hwmon"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(u32::MAX);
        chips.push((index, dir, name.trim().to_string()));
    }
    chips.sort();
    let mut sensors = Vec::new();
    for (_, dir, chip) in &chips {
        let mut inputs: Vec<(u32, String)> = Vec::new();
        let Ok(files) = std::fs::read_dir(dir) else { continue };
        for f in files.flatten() {
            let Ok(name) = f.file_name().into_string() else { continue };
            let Some(stem) = name.strip_prefix("temp").and_then(|s| s.strip_suffix("_input"))
            else {
                continue;
            };
            let index = stem.parse().unwrap_or(u32::MAX);
            inputs.push((index, name));
        }
        inputs.sort();
        for (_, input) in inputs {
            let stem = input.strip_suffix("_input").unwrap_or(&input);
            let label = std::fs::read_to_string(dir.join(format!("{stem}_label")))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| stem.to_string());
            // Same millidegree read path the control loop uses; best-effort.
            let current_c = HwmonSensor { input_path: dir.join(&input) }.read_c().ok();
            sensors.push(SensorInfo {
                chip: chip.clone(),
                label,
                spec: SensorSpec { hwmon_name: chip.clone(), input },
                current_c,
            });
        }
    }
    Ok(sensors)
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

    /// The current smoothed value (None until the first plausible reading).
    pub fn value(&self) -> Option<f32> {
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
        assert_eq!(ema.value(), None, "no reading yet");
        assert_eq!(ema.update(40.0), Some(40.0)); // first reading adopted
        let v = ema.update(50.0).unwrap(); // 0.3*50 + 0.7*40 = 43
        assert!((v - 43.0).abs() < 0.001);
        // implausible readings keep the previous value
        assert!((ema.update(-5.0).unwrap() - 43.0).abs() < 0.001);
        assert!((ema.update(400.0).unwrap() - 43.0).abs() < 0.001);
        // value() exposes the same smoothed reading update() returned
        assert!((ema.value().unwrap() - 43.0).abs() < 0.001);
    }

    #[test]
    fn enumerate_lists_channels_labels_and_current_temps() {
        let dir = tempfile::tempdir().unwrap();
        // hwmon0: nvme, temp1 without a label → label falls back to "temp1"
        fake_hwmon(dir.path(), 0, "nvme", 35_000);
        // hwmon3: k10temp with two labeled channels
        fake_hwmon(dir.path(), 3, "k10temp", 41_250);
        let d3 = dir.path().join("hwmon3");
        std::fs::write(d3.join("temp1_label"), "Tctl\n").unwrap();
        std::fs::write(d3.join("temp3_input"), "55000\n").unwrap();
        std::fs::write(d3.join("temp3_label"), "Tccd1\n").unwrap();
        // hwmon5: a chip whose temp file cannot be read — unreadable perms for
        // normal users, unparseable content as the fallback if running as root.
        fake_hwmon(dir.path(), 5, "spd5118", 0);
        let bad = dir.path().join("hwmon5").join("temp1_input");
        std::fs::write(&bad, "not-a-temp\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000)).unwrap();
        // hwmon7: no `name` file → unaddressable by SensorSpec → skipped
        let anon = dir.path().join("hwmon7");
        std::fs::create_dir_all(&anon).unwrap();
        std::fs::write(anon.join("temp1_input"), "12000\n").unwrap();

        let list = enumerate(dir.path()).unwrap();
        let summary: Vec<(&str, &str, &str, &str)> = list
            .iter()
            .map(|s| {
                (s.chip.as_str(), s.label.as_str(), s.spec.hwmon_name.as_str(), s.spec.input.as_str())
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("nvme", "temp1", "nvme", "temp1_input"),
                ("k10temp", "Tctl", "k10temp", "temp1_input"),
                ("k10temp", "Tccd1", "k10temp", "temp3_input"),
                ("spd5118", "temp1", "spd5118", "temp1_input"),
            ]
        );
        assert!((list[0].current_c.unwrap() - 35.0).abs() < 0.001);
        assert!((list[1].current_c.unwrap() - 41.25).abs() < 0.001);
        assert!((list[2].current_c.unwrap() - 55.0).abs() < 0.001);
        assert_eq!(list[3].current_c, None, "unreadable temp must be null, not an error");
    }

    #[test]
    fn enumerated_specs_resolve_back_to_the_same_file() {
        // Feed every emitted spec back through resolve() against the same
        // tree: it must land on the exact input file it was enumerated from.
        let dir = tempfile::tempdir().unwrap();
        fake_hwmon(dir.path(), 0, "nvme", 35_000);
        fake_hwmon(dir.path(), 3, "k10temp", 41_250);
        std::fs::write(dir.path().join("hwmon3").join("temp3_input"), "55000\n").unwrap();

        let list = enumerate(dir.path()).unwrap();
        assert_eq!(list.len(), 3);
        for info in &list {
            let resolved = resolve(dir.path(), &info.spec)
                .unwrap_or_else(|e| panic!("emitted spec {:?} must resolve: {e}", info.spec));
            let hwmon_dir = if info.chip == "nvme" { "hwmon0" } else { "hwmon3" };
            assert_eq!(
                resolved.input_path,
                dir.path().join(hwmon_dir).join(&info.spec.input),
                "spec {:?} must resolve to the file it was enumerated from",
                info.spec
            );
            // Same file ⇒ same reading (all three temps are distinct).
            assert_eq!(resolved.read_c().ok(), info.current_c);
        }
    }
}
