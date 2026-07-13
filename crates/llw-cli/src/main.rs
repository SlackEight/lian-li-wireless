//! `llw` — hardware proof CLI for llw-protocol (M1).
//! One-shot operations; run with RUST_LOG=debug for wire-level tracing.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use llw_protocol::dongle::Dongle;
use llw_protocol::frames::{apply_pwm_constraints, pwm_frame};
use llw_protocol::record::DeviceRecord;

#[derive(Parser)]
#[command(name = "llw", about = "Lian Li wireless protocol proof tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Survey all RF channels (1-39) for master responses
    Scan,
    /// List wireless devices reported by the RX dongle
    Devices,
    /// Send CMD_RESET to the TX dongle (master may hop channels)
    Reset,
    /// Set fan PWM on a device
    SetPwm {
        /// Device index from `llw devices`
        index: u8,
        /// Duty cycle percent (0-100), applied to all fan slots
        percent: u8,
        /// Re-send every second until Ctrl+C (fans revert without keepalive)
        #[arg(long)]
        hold: bool,
    },
    /// Set a static color on a device (single-frame onboard upload)
    SetColor {
        /// Device index from `llw devices`
        index: u8,
        /// Hex color, e.g. FF0000
        color: String,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Command::Scan => scan(),
        Command::Devices => devices(),
        Command::Reset => reset(),
        Command::SetPwm { index, percent, hold } => set_pwm(index, percent, hold),
        Command::SetColor { index, color } => set_color(index, &color),
    }
}

fn scan() -> Result<()> {
    let mut dongle = open_dongle()?;
    println!("Surveying channels 1-39...");
    let hits = dongle.survey_channels()?;
    if hits.is_empty() {
        println!("No master answered on any channel. (Try `llw reset` first, and check nothing else has claimed the dongles.)");
    }
    for h in &hits {
        println!(
            "  channel {:>2}: master {}  fw={}",
            h.channel,
            mac_str(&h.mac),
            h.firmware.map_or("?".into(), |f| f.to_string()),
        );
    }
    Ok(())
}

fn devices() -> Result<()> {
    let mut dongle = open_dongle()?;
    if !dongle.has_rx() {
        bail!("RX dongle not found — cannot list devices");
    }
    let report = poll_devices(&mut dongle)?;
    match report.mobo_pwm {
        Some(pwm) => println!("Motherboard PWM: {pwm}/255"),
        None => println!("Motherboard PWM: unavailable"),
    }
    println!("{} device(s):", report.devices.len());
    for d in &report.devices {
        println!(
            "  [{}] {} — {} | ch={} rx={} fans={} rpm={:?} pwm={:?} fx={:02x?}",
            d.list_index,
            mac_str(&d.mac),
            d.kind.display_name(),
            d.channel,
            d.rx_type,
            d.fan_count,
            d.fan_rpms,
            d.current_pwm,
            d.effect_index,
        );
    }
    Ok(())
}

fn reset() -> Result<()> {
    let mut dongle = open_dongle()?;
    dongle.reset()?;
    println!("CMD_RESET sent. Master may hop channels — run `llw scan` to re-locate.");
    Ok(())
}

fn set_pwm(index: u8, percent: u8, hold: bool) -> Result<()> {
    if percent > 100 {
        bail!("percent must be 0-100");
    }
    let mut dongle = open_dongle()?;
    let master = dongle.discover_master().context("discovering master")?;
    println!("Master {} on channel {}", mac_str(&master.mac), master.channel);

    let device = find_device(&mut dongle, index)?;
    let raw = (percent as u16 * 255 / 100) as u8;
    let mut pwm = [raw; 4];
    apply_pwm_constraints(&mut pwm, device.kind, device.fan_count);
    // seq_index: position among bound devices + 1 (single-device systems: 1)
    let rf = pwm_frame(&device.mac, &master.mac, device.rx_type, master.channel,
                       index + 1, &pwm);

    loop {
        dongle.send_rf_frame(&rf, device.channel, device.rx_type)?;
        println!("PWM {pwm:?} → {} ({})", mac_str(&device.mac), device.kind.display_name());
        if !hold {
            if percent > 0 {
                println!("note: without --hold, fans revert to hardware default in ~seconds");
            }
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn set_color(index: u8, color: &str) -> Result<()> {
    let rgb = parse_hex_color(color)?;
    let mut dongle = open_dongle()?;
    let master = dongle.discover_master().context("discovering master")?;
    println!("Master {} on channel {}", mac_str(&master.mac), master.channel);
    let device = find_device(&mut dongle, index)?;

    let led_count = device.total_leds();
    if led_count == 0 {
        bail!("device reports 0 LEDs — unsupported kind?");
    }
    let frame: Vec<[u8; 3]> = vec![rgb; led_count as usize];
    let fx = dongle.upload_rgb(
        &device.mac, &master.mac, device.channel, device.rx_type,
        &[frame], 5000, 4, // interval_ms irrelevant for a 1-frame loop; 5000 matches upstream's static uploads
    )?;
    println!(
        "Static #{color} → {} ({}, {} LEDs), effect index {:02x?}",
        mac_str(&device.mac), device.kind.display_name(), led_count, fx,
    );
    Ok(())
}

fn open_dongle() -> Result<Dongle> {
    Dongle::open().context(
        "opening dongles (if lianli-daemon is running, stop it first: \
         systemctl --user stop lianli-watchdog.service lianli-daemon.service)",
    )
}

fn poll_devices(dongle: &mut Dongle) -> Result<llw_protocol::record::GetDevReport> {
    // GetDev can time out sporadically, and the list can be legitimately
    // empty right after a reset — retry both cases before giving up.
    let mut last_empty = None;
    let mut last_err = None;
    for _ in 0..5 {
        match dongle.get_dev() {
            Ok(r) if !r.devices.is_empty() => return Ok(r),
            Ok(r) => last_empty = Some(r),
            Err(e) => last_err = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    if let Some(r) = last_empty {
        return Ok(r); // responsive but no devices — report honestly
    }
    Err(last_err.unwrap().into())
}

fn find_device(dongle: &mut Dongle, index: u8) -> Result<DeviceRecord> {
    let report = poll_devices(dongle)?;
    report
        .devices
        .into_iter()
        .find(|d| d.list_index == index)
        .with_context(|| format!("no device at index {index} — run `llw devices`"))
}

fn parse_hex_color(s: &str) -> Result<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("color must be 6 hex digits, e.g. FF0000");
    }
    Ok([
        u8::from_str_radix(&s[0..2], 16)?,
        u8::from_str_radix(&s[2..4], 16)?,
        u8::from_str_radix(&s[4..6], 16)?,
    ])
}

fn mac_str(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}
