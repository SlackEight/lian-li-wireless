//! llw-daemon — reliability daemon for Lian Li wireless devices.
//! M2a stub: config tooling only. The supervisor loop lands in M2b.

mod config;
#[allow(dead_code)] // used by the M2b supervisor
mod curve;
#[allow(dead_code)] // used by the M2b supervisor
mod fan;
mod migrate;
#[allow(dead_code)] // used by the M2b supervisor
mod reliability;
#[allow(dead_code)] // used by the M2b supervisor
mod sensors;

use anyhow::{Context as _, Result};

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
            report.config
                .validate()
                .context("imported config failed validation — fix the source config or file a bug")?;
            report.config.save(&dst)?;
            println!(
                "Imported {} curve(s), {} device(s) → {}",
                report.config.curves.len(),
                report.config.devices.len(),
                dst.display()
            );
            Ok(())
        }
        _ => {
            eprintln!("llw-daemon (M2a): supervisor not yet implemented.");
            eprintln!("usage: llw-daemon --check-config | --import-lianli [path] [--force]");
            std::process::exit(2);
        }
    }
}
