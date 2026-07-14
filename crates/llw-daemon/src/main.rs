//! llw-daemon — reliability daemon for Lian Li wireless devices.

mod acquisition;
mod config;
mod effects_bridge;
mod observation;
mod curve;
mod fan;
mod ipc;
mod migrate;
mod reliability;
mod rgb_assert;
mod sensors;
mod supervisor;

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
        None => run_daemon(),
        Some(other) => {
            eprintln!("unknown argument {other:?}");
            eprintln!("usage: llw-daemon [--check-config | --import-lianli [path] [--force]]");
            std::process::exit(2);
        }
    }
}

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
        path,
    );
    sup.run(&shutdown);
    let _ = std::fs::remove_file(ipc::socket_path());
    Ok(())
}
