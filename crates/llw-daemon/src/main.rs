//! llw-daemon — reliability daemon for Lian Li wireless devices.
//! M2a stub: config tooling only. The supervisor loop lands in M2b.

mod config;

use anyhow::Result;

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
        _ => {
            eprintln!("llw-daemon (M2a): supervisor not yet implemented.");
            eprintln!("usage: llw-daemon --check-config");
            std::process::exit(2);
        }
    }
}
