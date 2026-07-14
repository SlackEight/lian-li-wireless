//! `llw` — hardware proof CLI for llw-protocol (M1).
//! One-shot operations; run with RUST_LOG=debug for wire-level tracing.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use llw_protocol::dongle::Dongle;
use llw_protocol::frames::{apply_pwm_constraints, pwm_frame};
use llw_protocol::record::DeviceRecord;
use llw_effects::{Direction, EffectKind, EffectSpec};

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
    /// Show llw-daemon status
    Status,
    /// List all available effect kinds with one-line descriptions
    Effects,
    /// Set an animated effect on a device via the daemon
    SetEffect {
        /// Device index (from `llw status`) or MAC address
        index_or_mac: String,
        /// Effect kind (e.g. ripple, rainbow, breathing, static, ...)
        kind: String,
        /// Palette colors as comma-separated hex values (e.g. 0000FF,8800FF)
        #[arg(long)]
        colors: Option<String>,
        /// Animation speed 1-5 (default: 3)
        #[arg(long, default_value_t = 3)]
        speed: u8,
        /// Animation direction: forward or reverse (default: forward)
        #[arg(long, default_value = "forward")]
        direction: String,
        /// Brightness 0-4 (default: 4)
        #[arg(long, default_value_t = 4)]
        brightness: u8,
    },
    /// Bind an unbound wireless device to this controller via the daemon
    Bind {
        /// MAC address of the device to bind (e.g. aa:bb:cc:dd:ee:ff)
        mac: String,
    },
    /// Unbind a wireless device from this controller via the daemon
    Unbind {
        /// MAC address of the device to unbind (e.g. aa:bb:cc:dd:ee:ff)
        mac: String,
    },
    /// Poll devices continuously, printing telemetry (Ctrl+C to stop)
    Watch {
        /// Poll interval in milliseconds
        #[arg(long, default_value_t = 500)]
        interval_ms: u64,
        /// Also command this PWM percent to ALL devices each second
        /// (makes dropouts observable: readback should track this value)
        #[arg(long)]
        pwm: Option<u8>,
    },
    /// Reveal physical LED wiring order by chasing a white block across fan 0's LEDs (diagnostic; requires llw-daemon stopped)
    ProbeChase {
        /// Device index from `llw devices`
        index: u8,
        /// Frame interval in milliseconds (how long each block stays lit)
        #[arg(long, default_value_t = 400)]
        ms: u16,
        /// Number of consecutive LEDs lit per frame
        #[arg(long, default_value_t = 1)]
        block: u8,
    },
    /// Render a Rainbow at N frames, upload directly, then verify the firmware echoes the effect index (diagnostic; requires llw-daemon stopped)
    ProbeFrames {
        /// Device index from `llw devices`
        index: u8,
        /// Number of animation frames to render and upload
        #[arg(long)]
        frames: u16,
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
        Command::Status => status(),
        Command::Effects => effects(),
        Command::SetEffect { index_or_mac, kind, colors, speed, direction, brightness } => {
            set_effect_ipc(&index_or_mac, &kind, colors.as_deref(), speed, &direction, brightness)
        }
        Command::Bind { mac } => bind_ipc(&mac),
        Command::Unbind { mac } => unbind_ipc(&mac),
        Command::Watch { interval_ms, pwm } => watch(interval_ms, pwm),
        Command::ProbeChase { index, ms, block } => probe_chase(index, ms, block),
        Command::ProbeFrames { index, frames } => probe_frames(index, frames),
    }
}

fn status() -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("llw-daemon.sock");
    let mut stream = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connecting {} — is llw-daemon running?", path.display()))?;
    writeln!(stream, r#"{{"v":1,"method":"Status"}}"#)?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    if !v["ok"].as_bool().unwrap_or(false) {
        bail!("daemon error: {}", v["error"].as_str().unwrap_or("unknown"));
    }
    let d = &v["data"];
    println!("llw-daemon {}", d["daemon_version"].as_str().unwrap_or("?"));
    match d["link"].as_object() {
        Some(l) => println!(
            "link: master {} channel {}",
            l["master_mac"].as_str().unwrap_or("?"),
            l["channel"]
        ),
        None => println!("link: NOT ACQUIRED"),
    }
    if d["tx_wedged"].as_bool().unwrap_or(false) {
        println!("!! TX dongle missing/wedged — power-cycle may be required");
    }
    let r = &d["reliability"];
    println!(
        "reliability: dropouts={} tier1={} tier2={} streak={}",
        r["total_dropouts"], r["total_tier1"], r["total_tier2"], r["failed_tier1_streak"]
    );
    for dev in d["devices"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "  {} {} ch={} rpm={} desired={} readback={} rgb_sync={} streak={}",
            dev["mac"].as_str().unwrap_or("?"),
            dev["kind"].as_str().unwrap_or("?"),
            dev["channel"],
            dev["rpm"],
            dev["desired_pwm"],
            dev["readback_pwm"],
            dev["rgb_in_sync"],
            dev["dropout_streak"],
        );
    }
    // "on air:" section: non-Ours entries from the air inventory.
    let empty_air = Vec::new();
    let air: Vec<&serde_json::Value> = d["air"]
        .as_array()
        .unwrap_or(&empty_air)
        .iter()
        .filter(|e| e["bond"].as_str().unwrap_or("") != "Ours")
        .collect();
    if !air.is_empty() {
        println!("on air:");
        for e in &air {
            println!(
                "  {} {} {} ch={}",
                e["bond"].as_str().unwrap_or("?"),
                e["mac"].as_str().unwrap_or("?"),
                e["kind"].as_str().unwrap_or("?"),
                e["channel"],
            );
        }
    }
    Ok(())
}

/// Print all effect kinds with their one-line descriptions.
/// Safe to run without a daemon — no IPC contact.
fn effects() -> Result<()> {
    println!("Available effects:");
    for kind in EffectKind::all() {
        let name = serde_json::to_string(kind)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        println!("  {:<16} {}", name, kind.describe());
    }
    Ok(())
}

/// Send a `SetEffect` request to the running daemon.
/// `index_or_mac`: either a numeric device index (resolved to MAC via Status)
/// or a raw MAC string (used directly).
fn set_effect_ipc(
    index_or_mac: &str,
    kind_str: &str,
    colors_str: Option<&str>,
    speed: u8,
    direction_str: &str,
    brightness: u8,
) -> Result<()> {
    // Parse kind from kebab-case name via serde.
    let kind: EffectKind = serde_json::from_str(&format!(r#""{kind_str}""#))
        .with_context(|| {
            let valid: Vec<String> = EffectKind::all()
                .iter()
                .map(|k| {
                    serde_json::to_string(k)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string()
                })
                .collect();
            format!(
                "unknown effect kind {:?}; valid kinds: {}",
                kind_str,
                valid.join(", ")
            )
        })?;

    // Parse colors.
    let colors: Vec<[u8; 3]> = match colors_str {
        None => vec![],
        Some(s) => s
            .split(',')
            .map(|c| parse_hex_color(c.trim()))
            .collect::<Result<Vec<_>>>()?,
    };

    // Parse direction.
    let direction = match direction_str.to_lowercase().as_str() {
        "forward" => Direction::Forward,
        "reverse" => Direction::Reverse,
        other => bail!("direction must be 'forward' or 'reverse', got {:?}", other),
    };

    let spec = EffectSpec { kind, colors, speed, direction, brightness };

    // Resolve index → MAC via a Status IPC call, or use the string directly.
    let mac = resolve_mac_via_ipc(index_or_mac)?;

    // Build and send the SetEffect request.
    let req = serde_json::json!({
        "v": 1,
        "method": "SetEffect",
        "mac": mac,
        "effect": spec,
    });

    let resp = ipc_request(&req)?;
    if !resp["ok"].as_bool().unwrap_or(false) {
        bail!("daemon error: {}", resp["error"].as_str().unwrap_or("unknown"));
    }
    println!("Effect {:?} set on {}", kind_str, mac);
    Ok(())
}

/// Validate that `s` is a parseable MAC address (6 colon-separated hex octets).
/// Accepted formats: `aa:bb:cc:dd:ee:ff` (lowercase/uppercase, no normalization).
fn validate_mac(s: &str) -> Result<()> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        bail!("invalid MAC {:?} — expected 6 colon-separated hex octets (e.g. aa:bb:cc:dd:ee:ff)", s);
    }
    for p in &parts {
        if p.len() != 2 || !p.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("invalid MAC {:?} — octet {:?} is not two hex digits", s, p);
        }
    }
    Ok(())
}

/// The error string returned by the daemon when the radio is still settling
/// after a recent bind/unbind or RGB upload. Transient; auto-retried.
const SETTLING_ERROR: &str = "radio settling";

/// Send a Bind IPC request; on `{"state":"started"}` poll until bound or
/// timeout. Auto-retries the "radio settling" transient up to 3 times.
fn bind_ipc(mac: &str) -> Result<()> {
    validate_mac(mac)?;
    let mac_norm = mac.to_lowercase();
    let req = serde_json::json!({"v": 1, "method": "Bind", "mac": mac_norm});
    let resp = ipc_request_with_retry(&req)?;
    if !resp["ok"].as_bool().unwrap_or(false) {
        bail!("{}", resp["error"].as_str().unwrap_or("daemon error"));
    }
    // Accepted — poll Status until converged or timeout.
    poll_bind_convergence(&mac_norm, false)
}

/// Send an Unbind IPC request; mirror of bind_ipc.
fn unbind_ipc(mac: &str) -> Result<()> {
    validate_mac(mac)?;
    let mac_norm = mac.to_lowercase();
    let req = serde_json::json!({"v": 1, "method": "Unbind", "mac": mac_norm});
    let resp = ipc_request_with_retry(&req)?;
    if !resp["ok"].as_bool().unwrap_or(false) {
        bail!("{}", resp["error"].as_str().unwrap_or("daemon error"));
    }
    poll_bind_convergence(&mac_norm, true)
}

/// Send a request, auto-retrying on the "radio settling" transient error
/// up to 3 times with 2s gaps before surfacing it.
fn ipc_request_with_retry(req: &serde_json::Value) -> Result<serde_json::Value> {
    let mut last_resp = None;
    for attempt in 0..3u32 {
        let resp = ipc_request(req)?;
        let is_settling = !resp["ok"].as_bool().unwrap_or(false)
            && resp["error"]
                .as_str()
                .unwrap_or("")
                .contains(SETTLING_ERROR);
        if !is_settling {
            return Ok(resp);
        }
        if attempt < 2 {
            eprintln!("radio settling, retrying in 2s… (attempt {}/3)", attempt + 1);
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        last_resp = Some(resp);
    }
    Ok(last_resp.unwrap())
}

/// Poll Status every 500ms for up to 12s, printing progress from `pending`.
/// For bind: success = mac appears in `devices` (bound + configured).
/// For unbind: success = mac absent from both `devices` and `pending`.
fn poll_bind_convergence(mac: &str, is_unbind: bool) -> Result<()> {
    let op_name = if is_unbind { "unbind" } else { "bind" };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    let status_req = serde_json::json!({"v": 1, "method": "Status"});
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let resp = ipc_request(&status_req)?;
        if !resp["ok"].as_bool().unwrap_or(false) {
            bail!("daemon error polling status: {}", resp["error"].as_str().unwrap_or("unknown"));
        }
        let d = &resp["data"];
        let pending = &d["pending"];

        // Check if the pending op has failed.
        if !pending.is_null() {
            let op = pending["op"].as_str().unwrap_or("");
            let pending_mac = pending["mac"].as_str().unwrap_or("");
            let state = pending["state"].as_str().unwrap_or("");
            if op == op_name && pending_mac == mac {
                if state == "failed" {
                    bail!(
                        "{} failed — device did not converge; check llw status",
                        op_name
                    );
                }
                // Still converging — print progress.
                println!("converging…");
            }
        }

        // Check final state.
        let devices = d["devices"].as_array();
        if is_unbind {
            // Unbind success: mac absent from devices list AND no pending op for this mac.
            let still_configured = devices
                .unwrap_or(&Vec::new())
                .iter()
                .any(|dv| dv["mac"].as_str().unwrap_or("") == mac);
            let still_pending = !pending.is_null()
                && pending["mac"].as_str().unwrap_or("") == mac
                && pending["state"].as_str().unwrap_or("") != "failed";
            if !still_configured && !still_pending {
                println!("unbound ✓");
                return Ok(());
            }
        } else {
            // Bind success: mac appears in devices list AND no pending op for this mac.
            let is_bound = devices
                .unwrap_or(&Vec::new())
                .iter()
                .any(|dv| dv["mac"].as_str().unwrap_or("") == mac);
            let still_pending = !pending.is_null()
                && pending["mac"].as_str().unwrap_or("") == mac
                && pending["state"].as_str().unwrap_or("") != "failed";
            if is_bound && !still_pending {
                println!("bound + configured ✓");
                return Ok(());
            }
        }

        if std::time::Instant::now() >= deadline {
            bail!("still converging — check llw status");
        }
    }
}

/// If `index_or_mac` parses as a `u8`, resolve it to a MAC via a Status call.
/// Otherwise treat it as a MAC string directly.
fn resolve_mac_via_ipc(index_or_mac: &str) -> Result<String> {
    if let Ok(idx) = index_or_mac.parse::<u8>() {
        // Numeric index: get Status and find the device at that position.
        let status_req = serde_json::json!({"v": 1, "method": "Status"});
        let resp = ipc_request(&status_req)?;
        if !resp["ok"].as_bool().unwrap_or(false) {
            bail!("daemon status error: {}", resp["error"].as_str().unwrap_or("unknown"));
        }
        let devices = resp["data"]["devices"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // Status devices are returned in HashMap iteration order; the index here
        // is the list position in the Status response (0-based), matching the
        // index printed by `llw status`.
        let dev = devices
            .get(idx as usize)
            .with_context(|| format!("no device at index {idx} — run `llw status`"))?;
        Ok(dev["mac"]
            .as_str()
            .with_context(|| "device in status missing mac field")?
            .to_string())
    } else {
        // Treat as a raw MAC address.
        Ok(index_or_mac.to_string())
    }
}

/// Send a JSON value over the IPC socket and return the parsed response.
fn ipc_request(req: &serde_json::Value) -> Result<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("llw-daemon.sock");
    let mut stream = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connecting {} — is llw-daemon running?", path.display()))?;
    let line = serde_json::to_string(req)?;
    writeln!(stream, "{line}")?;
    let mut resp_line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut resp_line)?;
    Ok(serde_json::from_str(&resp_line)?)
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

fn watch(interval_ms: u64, pwm: Option<u8>) -> Result<()> {
    if let Some(p) = pwm {
        if p > 100 {
            bail!("--pwm must be 0-100");
        }
    }
    let mut dongle = open_dongle()?;
    let master = dongle.discover_master().context("discovering master")?;
    println!(
        "Master {} on channel {} — watching every {}ms{} (Ctrl+C to stop)",
        mac_str(&master.mac),
        master.channel,
        interval_ms,
        pwm.map_or(String::new(), |p| format!(", commanding {p}% PWM")),
    );

    let mut dropouts: u64 = 0;
    let mut polls: u64 = 0;
    let mut last_pwm_send = std::time::Instant::now() - std::time::Duration::from_secs(2);
    let started = std::time::Instant::now();

    loop {
        // 1Hz keepalive when commanding (best-effort: skip this second if the
        // pre-send poll fails — the main poll below reports the error).
        if let Some(p) = pwm {
            if last_pwm_send.elapsed() >= std::time::Duration::from_secs(1) {
                if let Ok(report) = dongle.get_dev() {
                    for d in &report.devices {
                        let raw = (p as u16 * 255 / 100) as u8;
                        let mut target = [raw; 4];
                        llw_protocol::frames::apply_pwm_constraints(&mut target, d.kind, d.fan_count);
                        let rf = pwm_frame(&d.mac, &master.mac, d.rx_type, master.channel,
                                           d.list_index + 1, &target);
                        let _ = dongle.send_rf_frame(&rf, d.channel, d.rx_type);
                    }
                    last_pwm_send = std::time::Instant::now();
                }
            }
        }

        match dongle.get_dev() {
            Ok(report) => {
                polls += 1;
                for d in &report.devices {
                    let commanded = pwm.is_some();
                    let dropped = commanded
                        && d.fan_count > 0
                        && d.current_pwm.iter().take(d.fan_count as usize).all(|&x| x == 0);
                    if dropped {
                        dropouts += 1;
                    }
                    println!(
                        "{:>7.1}s [{}] ch={} rpm={:?} pwm={:?} fx={:02x?}{}  (polls={} dropouts={})",
                        started.elapsed().as_secs_f32(),
                        d.list_index,
                        d.channel,
                        d.fan_rpms,
                        d.current_pwm,
                        d.effect_index,
                        if dropped { "  << DROPOUT" } else { "" },
                        polls,
                        dropouts,
                    );
                }
            }
            Err(e) => println!(
                "{:>7.1}s poll error: {e}  (polls={polls} dropouts={dropouts})",
                started.elapsed().as_secs_f32()
            ),
        }
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    }
}

/// Chase a white block through fan 0's LEDs to reveal physical wiring order.
///
/// Frame `i` lights LEDs `[i*block, (i+1)*block)` bright white; all other
/// LEDs on the device are black. The frame covers ALL device LEDs so the
/// upload targets the correct geometry.
///
/// For fan devices we chase fan 0 only (indices 0..leds_per_fan).
/// For flat-buffer devices (Strimer, Lc217, etc.) we chase up to 64 LEDs
/// from the start of the strip.
///
/// Run with llw-daemon stopped: `systemctl --user stop llw-daemon`.
fn probe_chase(index: u8, ms: u16, block: u8) -> Result<()> {
    if block == 0 {
        bail!("--block must be ≥ 1");
    }
    let mut dongle = open_dongle()?;
    let master = dongle.discover_master().context("discovering master")?;
    println!("Master {} on channel {}", mac_str(&master.mac), master.channel);

    let device = find_device(&mut dongle, index)?;
    let total = device.total_leds();
    if total == 0 {
        bail!("device reports 0 LEDs — unsupported kind?");
    }

    // Determine how many LEDs to chase (fan 0 for fan devices, up to 64 for strips).
    let leds_per_fan = device.kind.leds_per_fan();
    let (chase_count, target): (u16, &str) = if device.kind.led_count_override().is_some() {
        // Flat-buffer device: chase up to 64 LEDs
        (total.min(64), "the strip")
    } else if leds_per_fan > 0 {
        // Fan device: chase fan 0's LEDs only
        (leds_per_fan as u16, "fan 0")
    } else {
        bail!("cannot determine LED count per fan for this device kind");
    };

    let block = block as u16;
    let frame_count = chase_count.div_ceil(block);
    let loop_secs = frame_count as f32 * ms as f32 / 1000.0;

    println!(
        "chasing {chase_count} LEDs of {target} at {ms}ms — full loop {loop_secs:.1}s ({frame_count} frames, {block} LED(s)/frame)",
    );

    // Build one frame per block position. Each frame is total LEDs wide.
    let frames: Vec<Vec<[u8; 3]>> = (0..frame_count)
        .map(|i| {
            let start = (i * block) as usize;
            let end = ((i * block + block) as usize).min(total as usize);
            (0..total as usize)
                .map(|led| {
                    if led >= start && led < end {
                        [255u8, 255, 255]
                    } else {
                        [0u8, 0, 0]
                    }
                })
                .collect()
        })
        .collect();

    let fx = dongle.upload_rgb(
        &device.mac,
        &master.mac,
        device.channel,
        device.rx_type,
        &frames,
        ms,
        4,
    )?;
    println!(
        "uploaded {} frames → {} ({}, {} LEDs total), effect index {:02x?}",
        frames.len(),
        mac_str(&device.mac),
        device.kind.display_name(),
        total,
        fx,
    );
    Ok(())
}

/// Render a Rainbow animation at N frames and verify the firmware echoes the effect index.
///
/// Builds the device geometry directly from the device record (same logic as
/// `effects_bridge::geometry_of` in llw-daemon — duplicated here to keep this a
/// direct-dongle path with no daemon dependency), renders via `llw_effects::render_animation`,
/// uploads with `upload_rgb`, waits 3s, calls GetDev, and checks that the reported
/// `effect_index` matches the upload return value.
///
/// Run with llw-daemon stopped: `systemctl --user stop llw-daemon`.
fn probe_frames(index: u8, frames: u16) -> Result<()> {
    use llw_effects::{render_animation, Geometry};

    if frames == 0 {
        bail!("--frames must be ≥ 1");
    }

    let mut dongle = open_dongle()?;
    let master = dongle.discover_master().context("discovering master")?;
    println!("Master {} on channel {}", mac_str(&master.mac), master.channel);

    let device = find_device(&mut dongle, index)?;

    // Build geometry from the device record.
    // This mirrors effects_bridge::geometry_of — duplicated here so probe-frames
    // works as a direct-dongle path with no daemon crate dependency.
    // probe-frames is a diagnostic tool; UniformRing is adequate for frame-count probing.
    let geom = if device.kind.is_aio() {
        bail!("AIO devices are not supported by probe-frames (post-v1 geometry)");
    } else if let Some(total) = device.kind.led_count_override() {
        Geometry::Strip { total }
    } else {
        let lpf = device.kind.leds_per_fan();
        if lpf == 0 || device.fan_count == 0 {
            bail!(
                "cannot build geometry for {} (leds_per_fan={lpf}, fan_count={})",
                device.kind.display_name(),
                device.fan_count,
            );
        }
        use llw_effects::geometry::FanLayout;
        Geometry::Fans { fan_count: device.fan_count, leds_per_fan: lpf, layout: FanLayout::UniformRing }
    };

    let spec = EffectSpec {
        kind: EffectKind::Rainbow,
        colors: vec![],
        speed: 3,
        direction: Direction::Forward,
        brightness: 4,
    };

    let (rendered_frames, interval_ms) = render_animation(&spec, &geom, frames);

    let raw_bytes: usize = rendered_frames.iter().map(|f| f.len() * 3).sum();
    println!(
        "rendering Rainbow: {frames} frames × {} LEDs = {raw_bytes} raw bytes, interval {interval_ms}ms",
        geom.len(),
    );

    let upload_fx = dongle.upload_rgb(
        &device.mac,
        &master.mac,
        device.channel,
        device.rx_type,
        &rendered_frames,
        interval_ms,
        4,
    )?;
    println!(
        "uploaded → {} ({}), effect index {:02x?}",
        mac_str(&device.mac),
        device.kind.display_name(),
        upload_fx,
    );

    // Wait 3s of silence then verify the firmware echoes the effect index.
    println!("waiting 3s for firmware to settle...");
    std::thread::sleep(std::time::Duration::from_secs(3));

    let verify_report = poll_devices(&mut dongle)?;
    let dev_after = verify_report
        .devices
        .iter()
        .find(|d| d.mac == device.mac)
        .with_context(|| "device disappeared from GetDev after upload")?;

    if dev_after.effect_index == upload_fx {
        println!(
            "PASS — effect_index {:02x?} matches upload return value {:02x?}",
            dev_after.effect_index, upload_fx,
        );
    } else {
        println!(
            "FAIL — effect_index {:02x?} does not match upload return value {:02x?}",
            dev_after.effect_index, upload_fx,
        );
    }
    println!("fx values: {:02x?}", dev_after.effect_index);

    Ok(())
}
