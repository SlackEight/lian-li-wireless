//! Channel-move via re-bind: send a bind burst re-binding the device to its
//! CURRENT master and rx endpoint but with a different master_channel byte
//! (rf[15]), then persist with SaveConfig if the device follows. No unbind
//! first — if the firmware ignores the channel field this is a no-op re-bind.
//!
//! Usage: cargo run -p llw-protocol --example rebind-channel -- <target 1-39>
//! REQUIRES llw-daemon stopped.

use llw_protocol::dongle::Dongle;
use llw_protocol::frames::bind_frame;
use std::{thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target: u8 = std::env::args()
        .nth(1)
        .ok_or("usage: rebind-channel <target 1-39>")?
        .parse()?;
    assert!((1..=39).contains(&target));
    let sock = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default() + "/llw-daemon.sock";
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return Err("llw-daemon is running — stop it first".into());
    }

    let mut dongle = Dongle::open()?;
    let rec = loop {
        if let Ok(rep) = dongle.get_dev() {
            if let Some(r) = rep.devices.first() {
                break r.clone();
            }
        }
        thread::sleep(Duration::from_millis(300));
    };
    println!(
        "device {} master {:02x?} rx_type {} on ch{}",
        rec.mac_str(),
        rec.master_mac,
        rec.rx_type,
        rec.channel
    );
    if rec.channel == target {
        return Err("already on target".into());
    }

    let pwm = [86, 86, 86, 0];
    let rf = bind_frame(&rec.mac, &rec.master_mac, rec.rx_type, target, &pwm);
    println!("re-bind burst: same master/rx, master_channel={target}, tx on ch{}", rec.channel);
    dongle.send_bind_burst(&rf, rec.channel, rec.rx_type)?;

    // Convergence poll ≤6s: did the record's channel follow?
    let mut moved = false;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(150));
        if let Ok(rep) = dongle.get_dev() {
            if let Some(r) = rep.devices.first() {
                if r.channel == target {
                    moved = true;
                    break;
                }
            }
        }
    }

    if moved {
        println!("MOVED: device reports ch{target} — persisting with SaveConfig");
        dongle.send_save_config(&rec.master_mac, target)?;
        thread::sleep(Duration::from_secs(3)); // RF settle for the flash commit
        let rep = dongle.get_dev()?;
        if let Some(r) = rep.devices.first() {
            println!("final: channel={} pwm={:?} rpm={:?}", r.channel, r.current_pwm, r.fan_rpms);
        }
    } else {
        let now = dongle
            .get_dev()
            .ok()
            .and_then(|rep| rep.devices.first().map(|r| r.channel))
            .unwrap_or(0);
        println!("NOT MOVED: device still reports ch{now} — channel byte ignored at re-bind (no-op; nothing changed)");
    }
    Ok(())
}
