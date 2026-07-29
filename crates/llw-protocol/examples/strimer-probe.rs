//! Strimer mapping probe — light specific LEDs and ask the owner what they see.
//!
//! Three things are unknown about the Strimers and all of them are cheap to
//! settle by eye:
//!   1. WHICH physical cable is which MAC.
//!   2. HOW LED indices map onto the ribbon: are they numbered along one light
//!      guide and then onto the next (strand-major), or across all guides at
//!      each position along the cable (position-major)?
//!   3. WHETHER the LED count is real. `device_kind::led_count_override` is a
//!      hardcoded table keyed on a device_type byte (1=>116, 2=>132, 3=>174),
//!      not something the device reports — so it may simply be wrong, which
//!      would explain both 174 not dividing by 12 guides and the silent
//!      over-budget failures.
//!
//! Usage: strimer-probe <pattern> [args]
//!   list                     what is on the air, with assumed LED counts
//!   identify                 strimer A red, strimer B blue, everything else off
//!   pixel   <a|b> <index>    ONE led lit white — "where is it?"
//!   blocks  <a|b> [size]     consecutive runs in cycling colours (default 11)
//!   ends    <a|b> <n>        first n LEDs red, last n LEDs blue
//!   beyond  <a|b> <from>     light only indices >= from (does the tail exist?)
//!   off                      everything dark
//!
//! REQUIRES llw-daemon stopped. Restart it to restore the configured look.

use llw_protocol::dongle::Dongle;
use llw_protocol::record::DeviceRecord;
use std::time::Duration;

const OFF: [u8; 3] = [0, 0, 0];
const BLOCK_COLOURS: [[u8; 3]; 6] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 90, 255],
    [255, 200, 0],
    [255, 255, 255],
    [255, 0, 220],
];

fn strimers(devs: &[DeviceRecord]) -> Vec<DeviceRecord> {
    let mut v: Vec<_> = devs.iter().filter(|r| r.fan_count == 0).cloned().collect();
    v.sort_by_key(|r| r.mac);
    v
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

/// Push one static frame and VERIFY it actually took, retrying if not.
///
/// `06:ed` in particular drops uploads regularly — it failed at every gap in
/// the 2026-07-29 settle experiment, and it is the device the owner reported
/// stuck on static while the other ran meteor. Firing an upload and hoping is
/// exactly why that looked like "the effect will not change": nothing retries.
/// `upload_rgb` returns the effect index the device should echo once it has
/// committed, so we can poll for it and re-send until it matches.
fn show(
    dongle: &mut Dongle,
    rec: &DeviceRecord,
    master: &[u8; 6],
    frame: Vec<[u8; 3]>,
) -> Result<(), Box<dyn std::error::Error>> {
    for attempt in 1..=4 {
        let want = dongle.upload_rgb(&rec.mac, master, rec.channel, rec.rx_type,
                                     std::slice::from_ref(&frame), 1000, 4)?;
        std::thread::sleep(Duration::from_millis(1100));
        for _ in 0..24 {
            if let Ok(rep) = dongle.get_dev() {
                if rep.devices.iter().any(|r| r.mac == rec.mac && r.effect_index == want) {
                    if attempt > 1 {
                        eprintln!("  ({} took {attempt} attempts)", rec.mac_str());
                    }
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("  ({} did not confirm, attempt {attempt} — resending)", rec.mac_str());
    }
    Err(format!("{} never confirmed the frame after 4 attempts", rec.mac_str()).into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pattern = args.first().map(String::as_str).unwrap_or("list");

    let sock = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default() + "/llw-daemon.sock";
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        return Err("llw-daemon is running — stop it first".into());
    }
    let mut dongle = Dongle::open()?;
    let devices = snapshot(&mut dongle);
    if devices.is_empty() {
        return Err("no devices enumerated".into());
    }
    let master = devices[0].master_mac;
    let strs = strimers(&devices);
    if strs.is_empty() {
        return Err("no Strimers on the air".into());
    }

    let pick = |which: Option<&String>| -> Result<DeviceRecord, String> {
        match which.map(|s| s.as_str()) {
            Some("a") | None => Ok(strs[0].clone()),
            Some("b") => strs
                .get(1)
                .cloned()
                .ok_or_else(|| "only one Strimer on the air".to_string()),
            Some(other) => Err(format!("pick a or b, not {other}")),
        }
    };

    match pattern {
        "list" => {
            println!("Strimers on the air (assumed counts — that is what we are testing):");
            for (i, r) in strs.iter().enumerate() {
                println!(
                    "  {}  {}  device_type {}  assumed {} LEDs",
                    if i == 0 { "a" } else { "b" },
                    r.mac_str(),
                    r.device_type,
                    r.total_leds()
                );
            }
            println!("\nOther devices: {}", devices.len() - strs.len());
        }

        "identify" => {
            for (i, r) in strs.iter().enumerate() {
                let colour = if i == 0 { [255, 0, 0] } else { [0, 90, 255] };
                show(&mut dongle, r, &master, vec![colour; r.total_leds() as usize])?;
            }
            for r in devices.iter().filter(|r| r.fan_count > 0) {
                show(&mut dongle, r, &master, vec![OFF; r.total_leds() as usize])?;
            }
            println!("Strimer a = RED, Strimer b = BLUE, fans dark.");
            println!("=> Which one is the 24-pin, and which is the GPU cable?");
        }

        "pixel" => {
            let rec = pick(args.get(1))?;
            let idx: usize = args.get(2).ok_or("need an index")?.parse()?;
            let n = rec.total_leds() as usize;
            if idx >= n {
                println!("note: index {idx} is beyond the assumed count {n} — if it lights, the count is wrong");
            }
            let mut frame = vec![OFF; n.max(idx + 1)];
            frame[idx] = [255, 255, 255];
            show(&mut dongle, &rec, &master, frame)?;
            println!("LED {idx} lit white on {}.", rec.mac_str());
            println!("=> Where is it? (which end, which guide, how far along)");
        }

        "blocks" => {
            let rec = pick(args.get(1))?;
            let size: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(11);
            let n = rec.total_leds() as usize;
            let frame: Vec<[u8; 3]> = (0..n)
                .map(|i| BLOCK_COLOURS[(i / size) % BLOCK_COLOURS.len()])
                .collect();
            show(&mut dongle, &rec, &master, frame)?;
            println!("{} lit in consecutive runs of {size}: red, green, blue, amber, white, pink…", rec.mac_str());
            println!("=> Do the colours run ALONG the cable as long stripes (one colour per");
            println!("   light guide), or ACROSS it as short bands (all guides changing together)?");
        }

        "ends" => {
            let rec = pick(args.get(1))?;
            let k: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(6);
            let n = rec.total_leds() as usize;
            let frame: Vec<[u8; 3]> = (0..n)
                .map(|i| {
                    if i < k {
                        [255, 0, 0]
                    } else if i + k >= n {
                        [0, 90, 255]
                    } else {
                        OFF
                    }
                })
                .collect();
            show(&mut dongle, &rec, &master, frame)?;
            println!("{}: first {k} LEDs RED, last {k} BLUE, middle dark.", rec.mac_str());
            println!("=> Is red at the connector end or the far end? Is blue at the opposite end,");
            println!("   or does it fall short (which would mean the real count is lower than {n})?");
        }

        "beyond" => {
            let rec = pick(args.get(1))?;
            let from: usize = args.get(2).ok_or("need a start index")?.parse()?;
            // Send EXACTLY the device's own count. Padding past it made the
            // device refuse the frame outright (186 into a 174-LED device,
            // four rejections) — which is itself a finding: over-length
            // uploads are rejected, not silently truncated.
            let n = rec.total_leds() as usize;
            let frame: Vec<[u8; 3]> = (0..n)
                .map(|i| if i >= from { [0, 255, 120] } else { OFF })
                .collect();
            show(&mut dongle, &rec, &master, frame)?;
            println!("{}: indices {from}..{n} lit green, everything before dark.", rec.mac_str());
            println!("=> Does ANY of it light? How much of the cable does it cover?");
        }

        "off" => {
            for r in &devices {
                show(&mut dongle, r, &master, vec![OFF; r.total_leds() as usize])?;
            }
            println!("all dark");
        }

        other => return Err(format!("unknown pattern {other}").into()),
    }

    Ok(())
}
