//! One-off channel-steering probe v2 — answers M2a's deferred Q2 for real.
//!
//! v1 finding (2026-07-17): rf[15] ("master channel") alone does NOT move the
//! network — 6 bursts on the current channel + direct-channel frames left the
//! device record on its channel. FALSIFIED.
//!
//! v2 theory, from the recorded June boot-lock mechanism: upstream's 8-FIRST
//! GET_MAC scan is what locked the master onto its transient boot channel —
//! i.e. the master follows the host's PHYSICAL transmit channel under
//! sustained traffic (GET_MAC pings answer on any channel, and consistent
//! host traffic on one channel pins the network there). Steering = sustained
//! GET_MAC(target) pings + keepalives transmitted ON the target channel.
//!
//! Usage: cargo run -p llw-protocol --example steer-probe -- <target 1-39>
//! REQUIRES llw-daemon stopped (it owns the dongles otherwise).

use llw_protocol::dongle::Dongle;
use llw_protocol::frames::pwm_frame;
use std::{thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target: u8 = std::env::args()
        .nth(1)
        .ok_or("usage: steer-probe <target-channel 1-39>")?
        .parse()?;
    assert!((1..=39).contains(&target), "channel must be 1-39");

    // Refuse to fight the daemon for the dongle.
    let sock = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default() + "/llw-daemon.sock";
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return Err("llw-daemon is running — stop it first (systemctl --user stop llw-daemon)".into());
    }

    let mut dongle = Dongle::open()?;
    // The storm's deaf windows can eat individual polls — retry the opener.
    let mut first = None;
    for _ in 0..10 {
        match dongle.get_dev() {
            Ok(rep) if !rep.devices.is_empty() => {
                first = Some(rep);
                break;
            }
            _ => thread::sleep(Duration::from_millis(400)),
        }
    }
    let report = first.ok_or("GetDev unanswered after 10 tries")?;
    let rec = report.devices.first().unwrap().clone();
    println!(
        "before: mac={} channel={} rx_type={} pwm={:?} rpm={:?}",
        rec.mac_str(),
        rec.channel,
        rec.rx_type,
        rec.current_pwm,
        rec.fan_rpms
    );
    if rec.channel == target {
        return Err("device already on the target channel".into());
    }

    let pwm = if rec.current_pwm.iter().any(|&p| p != 0) {
        rec.current_pwm
    } else {
        [86, 86, 86, 0] // the config's known desired — mid-zero-window fallback
    };
    // Keepalive frame consistent with the target world: rf[15] = target too.
    let rf = pwm_frame(
        &rec.mac,
        &rec.master_mac,
        rec.rx_type,
        target,
        rec.list_index + 1,
        &pwm,
    );

    // Sustained steering traffic ON the target channel: GET_MAC(target) pings
    // (the primitive the June boot-lock used on ch8) interleaved with
    // keepalives, ~15s total, checking the device record every second.
    println!("steering: sustained GET_MAC({target}) + keepalives on ch{target}…");
    for round in 0..15 {
        let ping = dongle.get_mac(target);
        dongle.send_rf_frame(&rf, target, rec.rx_type)?;
        thread::sleep(Duration::from_millis(150));
        dongle.send_rf_frame(&rf, target, rec.rx_type)?;
        thread::sleep(Duration::from_millis(150));
        match dongle.get_dev() {
            Ok(rep) => {
                if let Some(r) = rep.devices.first() {
                    let mac_seen = matches!(&ping, Ok(Some(_)));
                    println!(
                        "  round {:2}: record channel={} pwm={:?} (get_mac answered: {})",
                        round + 1,
                        r.channel,
                        r.current_pwm,
                        mac_seen
                    );
                    if r.channel == target {
                        println!("STEERED: device now reports channel {target}");
                        // Confirm stability: 3s of keepalives on target, re-check.
                        for _ in 0..10 {
                            dongle.send_rf_frame(&rf, target, rec.rx_type)?;
                            thread::sleep(Duration::from_millis(300));
                        }
                        let fin = dongle.get_dev()?;
                        let fr = fin.devices.first().ok_or("device lost post-steer")?;
                        println!(
                            "final: channel={} pwm={:?} rpm={:?}",
                            fr.channel, fr.current_pwm, fr.fan_rpms
                        );
                        return Ok(());
                    }
                }
            }
            Err(e) => println!("  round {:2}: GetDev error: {e}", round + 1),
        }
    }

    // Restore normalcy: reassert the original channel + PWM.
    let restore = pwm_frame(
        &rec.mac,
        &rec.master_mac,
        rec.rx_type,
        rec.channel,
        rec.list_index + 1,
        &pwm,
    );
    for _ in 0..5 {
        let _ = dongle.get_mac(rec.channel);
        dongle.send_rf_frame(&restore, rec.channel, rec.rx_type)?;
        thread::sleep(Duration::from_millis(200));
    }
    let rep = dongle.get_dev()?;
    if let Some(r) = rep.devices.first() {
        println!("NOT STEERED — record still channel={} (restored)", r.channel);
    }
    Ok(())
}
