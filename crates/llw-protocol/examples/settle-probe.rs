//! Settle-window experiment: how fast can we really push RGB to a whole rig?
//!
//! Our daemon holds ALL RF traffic quiet for 3s after every upload (M3's
//! `rf_settle_until`), and confirms via the device's echoed effect_index, which
//! together cost ~4-5s per device — ~22s for five. Upstream lian-li-linux has NO
//! settle window at all. This probe measures what the firmware actually needs.
//!
//! For each candidate inter-upload gap it uploads a DISTINCT static colour to
//! every device back to back, waits, then polls GetDev and checks that every
//! device echoes the effect index we computed for it. All devices matching =
//! that gap is sufficient. It also times the confirm latency separately.
//!
//! Usage: cargo run -p llw-protocol --example settle-probe
//! REQUIRES llw-daemon stopped. Restores nothing — the daemon re-asserts config
//! on restart.

use llw_protocol::dongle::Dongle;
use llw_protocol::record::DeviceRecord;
use std::time::{Duration, Instant};

/// Distinct probe colours, one per device.
const COLOURS: [[u8; 3]; 6] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 80, 255],
    [255, 160, 0],
    [255, 255, 255],
    [255, 0, 200],
];

/// `n_frames` frames of a moving comet — a realistic ANIMATION payload, not a
/// 14-byte static frame. This is the case M3's settle rule was born from.
fn frames_for(rec: &DeviceRecord, colour: [u8; 3], n_frames: usize) -> Vec<Vec<[u8; 3]>> {
    let leds = rec.total_leds() as usize;
    if n_frames <= 1 {
        return vec![vec![colour; leds]];
    }
    (0..n_frames)
        .map(|f| {
            let head = (f as f32 / n_frames as f32) * leds as f32;
            (0..leds)
                .map(|i| {
                    let mut d = (i as f32 - head).abs();
                    if d > leds as f32 / 2.0 {
                        d = leds as f32 - d;
                    }
                    let k = (-d * 0.35).exp();
                    [
                        (colour[0] as f32 * k) as u8,
                        (colour[1] as f32 * k) as u8,
                        (colour[2] as f32 * k) as u8,
                    ]
                })
                .collect()
        })
        .collect()
}

fn snapshot(dongle: &mut Dongle) -> Vec<DeviceRecord> {
    for _ in 0..12 {
        if let Ok(rep) = dongle.get_dev() {
            if !rep.devices.is_empty() {
                return rep.devices;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Vec::new()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sock = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default() + "/llw-daemon.sock";
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return Err("llw-daemon is running — stop it first".into());
    }
    let n_frames: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1);
    let mut dongle = Dongle::open()?;
    // Take the master + channel from the DEVICE RECORDS, not from a GET_MAC
    // channel scan: GET_MAC answers on every channel and first-hit discovery
    // returns junk (observed 2026-07-29 — it reported a master that does not
    // exist, so every upload was addressed into the void and nothing confirmed).
    let devices = snapshot(&mut dongle);
    if devices.is_empty() {
        return Err("no devices enumerated".into());
    }
    let master_mac = devices[0].master_mac;
    let channel = devices[0].channel;
    println!(
        "master {:02x?} ch{} · {} devices\n",
        master_mac,
        channel,
        devices.len()
    );
    // Per-device frame budget, exactly as llw-daemon's effects_bridge does:
    // clamp(28_000 / (leds * 3), 8, 96). Forcing one frame count across devices
    // pushes big-LED devices over their flash budget, where they fail SILENTLY
    // (observed 2026-07-29: a 174-LED Strimer rejected 70 frames at every gap).
    let budget = |leds: u16| -> usize {
        if n_frames == 1 { return 1; }
        ((28_000usize / (leds as usize * 3)).clamp(8, 96)).min(n_frames)
    };
    for r in &devices {
        println!(
            "  {} · {:<28} {:>3} leds · budget {:>2} frames · {:>6} B raw",
            r.mac_str(), r.kind.display_name(), r.total_leds(),
            budget(r.total_leds()),
            budget(r.total_leds()) * r.total_leds() as usize * 3
        );
    }
    println!();

    // Candidate inter-upload gaps, hardest (fastest) first so a pass is decisive.
    for gap_ms in [0u64, 150, 400, 1000, 3000] {
        // Rotate colours each round so a "pass" can't be a stale leftover.
        let round = (gap_ms as usize / 100) % COLOURS.len();
        let mut expected: Vec<([u8; 6], [u8; 4], String)> = Vec::new();

        let t0 = Instant::now();
        for (i, rec) in devices.iter().enumerate() {
            let colour = COLOURS[(i + round) % COLOURS.len()];
            let frames = frames_for(rec, colour, budget(rec.total_leds()));
            let idx = dongle.upload_rgb(
                &rec.mac,
                &master_mac,
                rec.channel,
                rec.rx_type,
                &frames,
                1000,
                4,
            )?;
            expected.push((rec.mac, idx, rec.mac_str()));
            if gap_ms > 0 && i + 1 < devices.len() {
                std::thread::sleep(Duration::from_millis(gap_ms));
            }
        }
        let upload_wall = t0.elapsed();

        // Give the last device the same grace every other device got, then poll
        // until everything confirms or we give up.
        std::thread::sleep(Duration::from_millis(gap_ms.max(150)));
        let mut confirmed_at: Option<Duration> = None;
        let mut last_state = String::new();
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline {
            if let Ok(rep) = dongle.get_dev() {
                let mut ok = 0;
                let mut state = String::new();
                for (mac, idx, name) in &expected {
                    let hit = rep
                        .devices
                        .iter()
                        .find(|r| &r.mac == mac)
                        .map(|r| r.effect_index == *idx)
                        .unwrap_or(false);
                    if hit {
                        ok += 1;
                    }
                    state.push_str(if hit { "✓" } else { "·" });
                    let _ = name;
                }
                last_state = state;
                if ok == expected.len() {
                    confirmed_at = Some(t0.elapsed());
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }

        match confirmed_at {
            Some(d) => println!(
                "gap {:>4}ms  PASS  uploads {:>5.2}s · all {} confirmed at {:>5.2}s   [{}]",
                gap_ms,
                upload_wall.as_secs_f32(),
                expected.len(),
                d.as_secs_f32(),
                last_state
            ),
            None => println!(
                "gap {:>4}ms  FAIL  uploads {:>5.2}s · not all confirmed in 12s      [{}]",
                gap_ms,
                upload_wall.as_secs_f32(),
                last_state
            ),
        }
        // Settle fully between rounds so one round can't poison the next.
        std::thread::sleep(Duration::from_secs(4));
    }

    println!("\ndone — restart llw-daemon to restore the configured look");
    Ok(())
}
