//! Dropout discriminator: sample GetDev at ~4Hz and print PWM readback, RPM,
//! and the master-measured motherboard PWM duty across zero-readback windows.
//! If mobo_pwm correlates with the ramps → the master is failing over to the
//! motherboard header (local cause). If it stays constant → external traffic.
//!
//! Usage: cargo run -p llw-protocol --example mobo-watch -- <seconds>
//! REQUIRES llw-daemon stopped.

use llw_protocol::dongle::Dongle;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secs: u64 = std::env::args().nth(1).map_or(60, |s| s.parse().unwrap_or(60));
    let sock = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default() + "/llw-daemon.sock";
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return Err("llw-daemon is running — stop it first".into());
    }

    let mut dongle = Dongle::open()?;
    let t0 = Instant::now();
    let mut last = String::new();
    while t0.elapsed() < Duration::from_secs(secs) {
        match dongle.get_dev() {
            Ok(rep) => {
                if let Some(r) = rep.devices.first() {
                    let line = format!(
                        "pwm={:?} rpm={:?} mobo_pwm={:?}",
                        r.current_pwm, r.fan_rpms, rep.mobo_pwm
                    );
                    if line != last {
                        println!("t={:6.2}s {line}", t0.elapsed().as_secs_f32());
                        last = line;
                    }
                }
            }
            Err(_) => {
                let line = "GetDev: no response".to_string();
                if line != last {
                    println!("t={:6.2}s {line}", t0.elapsed().as_secs_f32());
                    last = line;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}
