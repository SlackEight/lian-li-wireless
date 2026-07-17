//! Packet-loss discriminator: when a revert window opens, hammer correct
//! keepalive frames at 30ms and measure time-to-readback-recovery at ms
//! resolution. If the master accepts instantly and the 1-3s daemon-observed
//! recovery latency is RF packet loss, a dense burst should recover the
//! readback on the first post-burst poll.
//!
//! Usage: cargo run -p llw-protocol --example burst-recovery -- <windows>
//! REQUIRES llw-daemon stopped.

use llw_protocol::dongle::Dongle;
use llw_protocol::frames::pwm_frame;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target_windows: u32 = std::env::args().nth(1).map_or(3, |s| s.parse().unwrap_or(3));
    let sock = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default() + "/llw-daemon.sock";
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return Err("llw-daemon is running — stop it first".into());
    }

    let mut dongle = Dongle::open()?;
    // learn the device + assert a healthy 86 first
    let rec = loop {
        if let Ok(rep) = dongle.get_dev() {
            if let Some(r) = rep.devices.first() {
                break r.clone();
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    let pwm = [86, 86, 86, 0];
    let rf = pwm_frame(
        &rec.mac,
        &rec.master_mac,
        rec.rx_type,
        rec.channel, // rf[15] = the REAL channel this time
        rec.list_index + 1,
        &pwm,
    );
    println!("device {} on ch{}; waiting for revert windows…", rec.mac_str(), rec.channel);

    let mut seen = 0;
    let t0 = Instant::now();
    while seen < target_windows && t0.elapsed() < Duration::from_secs(120) {
        // keep the network fed while waiting (1s cadence like the daemon)
        dongle.send_rf_frame(&rf, rec.channel, rec.rx_type)?;
        let zeroed = match dongle.get_dev() {
            Ok(rep) => rep
                .devices
                .first()
                .is_some_and(|r| r.current_pwm[..3].iter().all(|&p| p == 0)),
            Err(_) => false,
        };
        if !zeroed {
            std::thread::sleep(Duration::from_millis(700));
            continue;
        }

        // WINDOW OPEN — burst keepalives at 30ms, poll every 3rd frame
        seen += 1;
        let w0 = Instant::now();
        let mut recovered_ms: Option<u128> = None;
        let mut frames_sent = 0;
        for i in 0..100 {
            dongle.send_rf_frame(&rf, rec.channel, rec.rx_type)?;
            frames_sent += 1;
            std::thread::sleep(Duration::from_millis(30));
            if i % 3 == 2 {
                if let Ok(rep) = dongle.get_dev() {
                    if let Some(r) = rep.devices.first() {
                        if r.current_pwm[..3].iter().any(|&p| p != 0) {
                            recovered_ms = Some(w0.elapsed().as_millis());
                            break;
                        }
                    }
                }
            }
        }
        // peak rpm shortly after
        std::thread::sleep(Duration::from_millis(1500));
        let peak = dongle
            .get_dev()
            .ok()
            .and_then(|rep| rep.devices.first().map(|r| *r.fan_rpms.iter().max().unwrap()))
            .unwrap_or(0);
        match recovered_ms {
            Some(ms) => println!(
                "window {seen}: recovered in {ms} ms ({frames_sent} frames sent), rpm@+1.5s={peak}"
            ),
            None => println!("window {seen}: NOT recovered after {frames_sent} frames (~3s)"),
        }
    }
    if seen == 0 {
        println!("no revert windows observed in 120s (keepalives at 1s held the master)");
    }
    Ok(())
}
