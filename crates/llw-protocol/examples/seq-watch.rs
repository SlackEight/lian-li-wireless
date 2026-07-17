//! Crash discriminator: sample GetDev at 4Hz and print cmd_seq alongside
//! PWM readback across zero-windows. If the master's RF MCU is REBOOTING,
//! cmd_seq (and other volatile counters) should reset/jump at each window;
//! an external state-write leaves it monotonic.
//!
//! Usage: cargo run -p llw-protocol --example seq-watch -- <seconds>
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
        let line = match dongle.get_dev() {
            Ok(rep) => match rep.devices.first() {
                Some(r) => format!(
                    "pwm={:?} seq={} fw_ish={} rpm={:?} mobo={:?}",
                    r.current_pwm, r.cmd_seq, r.device_type, r.fan_rpms, rep.mobo_pwm
                ),
                None => "empty report".into(),
            },
            Err(_) => "no response".into(),
        };
        if line != last {
            println!("t={:6.2}s {line}", t0.elapsed().as_secs_f32());
            last = line;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}
