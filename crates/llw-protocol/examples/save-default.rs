//! Persist a sane fallback PWM into device flash via SaveConfig (0x15).
//!
//! Theory: on PWM-state loss (external interference, missed keepalives) the
//! master reverts to its FLASH-SAVED speed. Our cluster's saved state dates
//! to its original L-Connect binding — apparently full speed — which is why
//! every dropout window audibly surges the fans. Snapshot the current sane
//! PWM (34%) into flash so state-loss reverts quietly instead.
//!
//! Sequence: assert PWM → short settle → SaveConfig broadcast (3×200ms, the
//! same call our bind flow ships) → verify readback intact.
//!
//! Usage: cargo run -p llw-protocol --example save-default -- <pwm 0-255>
//! REQUIRES llw-daemon stopped.

use llw_protocol::dongle::Dongle;
use llw_protocol::frames::pwm_frame;
use std::{thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pwm_byte: u8 = std::env::args()
        .nth(1)
        .ok_or("usage: save-default <pwm 0-255>")?
        .parse()?;

    let sock = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default() + "/llw-daemon.sock";
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return Err("llw-daemon is running — stop it first".into());
    }

    let mut dongle = Dongle::open()?;
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
        "before: mac={} channel={} pwm={:?} rpm={:?}",
        rec.mac_str(),
        rec.channel,
        rec.current_pwm,
        rec.fan_rpms
    );

    // Assert the PWM we want persisted; three sends so at least one lands
    // outside any deaf window.
    let mut pwm = [pwm_byte; 4];
    pwm[rec.fan_count as usize..].fill(0);
    let rf = pwm_frame(
        &rec.mac,
        &rec.master_mac,
        rec.rx_type,
        rec.channel,
        rec.list_index + 1,
        &pwm,
    );
    for _ in 0..3 {
        dongle.send_rf_frame(&rf, rec.channel, rec.rx_type)?;
        thread::sleep(Duration::from_millis(300));
    }

    // Wait until readback reflects the asserted PWM (don't snapshot a
    // zero-window into flash!).
    let mut confirmed = false;
    for _ in 0..20 {
        if let Ok(rep) = dongle.get_dev() {
            if let Some(r) = rep.devices.first() {
                if r.current_pwm[..r.fan_count as usize] == pwm[..r.fan_count as usize] {
                    confirmed = true;
                    break;
                }
            }
        }
        dongle.send_rf_frame(&rf, rec.channel, rec.rx_type)?;
        thread::sleep(Duration::from_millis(400));
    }
    if !confirmed {
        return Err("readback never confirmed the asserted PWM — refusing to SaveConfig".into());
    }
    println!("readback confirms pwm {pwm_byte} — sending SaveConfig broadcast");

    dongle.send_save_config(&rec.master_mac, rec.channel)?;
    thread::sleep(Duration::from_millis(500));

    let rep = dongle.get_dev()?;
    if let Some(r) = rep.devices.first() {
        println!("after: pwm={:?} rpm={:?}", r.current_pwm, r.fan_rpms);
    }
    println!("SaveConfig sent — flash snapshot should now hold pwm {pwm_byte}");
    Ok(())
}
