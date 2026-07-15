//! Versioned IPC: newline-delimited JSON over a Unix socket.
//! Envelope carries `v` (protocol version); unknown versions are rejected
//! with a structured error so mismatched daemon/CLI pairs fail actionably.

use crate::config::Config;
use crate::reliability::Telemetry;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use tracing::{info, warn};

pub const IPC_VERSION: u32 = 1;

pub fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("llw-daemon.sock")
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub v: u32,
    #[serde(flatten)]
    pub req: Request,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum Request {
    Ping,
    Status,
    GetConfig,
    SetConfig { config: Config },
    SetColor { mac: String, rgb: [u8; 3], brightness: u8 },
    SetEffect { mac: String, effect: llw_effects::EffectSpec },
    Bind { mac: String },
    Unbind { mac: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub v: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ResponseEnvelope {
    pub fn ok(data: Option<serde_json::Value>) -> Self {
        Self { v: IPC_VERSION, ok: true, error: None, data }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self { v: IPC_VERSION, ok: false, error: Some(msg.into()), data: None }
    }
}

/// The daemon's status snapshot served over IPC (and printed by `llw status`).
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusData {
    pub daemon_version: String,
    pub link: Option<LinkStatus>,
    pub tx_wedged: bool,
    pub reliability: Telemetry,
    pub devices: Vec<DeviceStatus>,
    pub air: Vec<AirDeviceStatus>,
    pub pending: Option<PendingOpStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingOpStatus {
    pub op: String,    // "bind" or "unbind"
    pub mac: String,
    pub state: String, // "converging" or "failed"
}

/// A device visible on the air (from the air inventory), including both
/// configured (Ours) and unconfigured (Foreign/Unbound) devices.
#[derive(Debug, Serialize, Deserialize)]
pub struct AirDeviceStatus {
    pub mac: String,
    pub kind: String,
    /// Bond classification: "Ours", "Foreign", or "Unbound"
    pub bond: String,
    pub channel: u8,
    pub fan_count: u8,
    pub rpm: [u16; 4],
    pub last_seen_s: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LinkStatus {
    pub master_mac: String,
    pub channel: u8,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub mac: String,
    pub kind: String,
    pub channel: u8,
    pub fan_count: u8,
    pub rpm: [u16; 4],
    pub desired_pwm: [u8; 4],
    pub readback_pwm: [u8; 4],
    pub rgb_in_sync: Option<bool>,
    pub dropout_streak: u32,
}

/// A request paired with its reply channel.
pub struct IpcCmd {
    pub req: Request,
    pub reply: mpsc::Sender<ResponseEnvelope>,
}

/// Bind the socket and serve connections forever, forwarding parsed requests
/// to the supervisor via `tx`. One thread per connection; one request per line.
pub fn serve(tx: mpsc::Sender<IpcCmd>) -> anyhow::Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path); // stale socket from a previous run
    let listener = UnixListener::bind(&path)?;
    info!("IPC listening on {}", path.display());
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let tx = tx.clone();
                std::thread::spawn(move || handle_conn(s, tx));
            }
            Err(e) => warn!("IPC accept failed: {e}"),
        }
    }
    Ok(())
}

fn handle_conn(stream: UnixStream, tx: mpsc::Sender<IpcCmd>) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;
    let mut line = String::new();
    while {
        line.clear();
        matches!(reader.read_line(&mut line), Ok(n) if n > 0)
    } {
        let resp = process_line(line.trim(), &tx);
        let Ok(json) = serde_json::to_string(&resp) else { break };
        if writeln!(writer, "{json}").is_err() {
            break;
        }
    }
}

pub(crate) fn process_line(line: &str, tx: &mpsc::Sender<IpcCmd>) -> ResponseEnvelope {
    let env: RequestEnvelope = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => return ResponseEnvelope::err(format!("bad request: {e}")),
    };
    if env.v != IPC_VERSION {
        return ResponseEnvelope::err(format!(
            "protocol version {} unsupported (daemon speaks {IPC_VERSION}) — update llw/llw-daemon",
            env.v
        ));
    }
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx.send(IpcCmd { req: env.req, reply: reply_tx }).is_err() {
        return ResponseEnvelope::err("daemon shutting down");
    }
    match reply_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(resp) => resp,
        Err(_) => ResponseEnvelope::err("daemon busy (no reply within 3s)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shapes() {
        let line = r#"{"v":1,"method":"Status"}"#;
        let env: RequestEnvelope = serde_json::from_str(line).unwrap();
        assert_eq!(env.v, 1);
        assert!(matches!(env.req, Request::Status));

        let line = r#"{"v":1,"method":"SetColor","mac":"02:8b:51:62:32:e1","rgb":[255,0,0],"brightness":4}"#;
        let env: RequestEnvelope = serde_json::from_str(line).unwrap();
        assert!(matches!(env.req, Request::SetColor { .. }));

        let resp = ResponseEnvelope::err("nope");
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""ok":false"#) && s.contains("nope") && !s.contains("data"));
    }

    #[test]
    fn set_effect_envelope_wire_shape() {
        // Full envelope with an explicit effect object.
        let line = r#"{"v":1,"method":"SetEffect","mac":"02:8b:51:62:32:e1","effect":{"kind":"ripple","colors":[[0,0,255]],"speed":3}}"#;
        let env: RequestEnvelope = serde_json::from_str(line).unwrap();
        assert_eq!(env.v, 1);
        match env.req {
            Request::SetEffect { mac, effect } => {
                assert_eq!(mac, "02:8b:51:62:32:e1");
                assert_eq!(effect.kind, llw_effects::EffectKind::Ripple);
                assert_eq!(effect.colors, vec![[0u8, 0, 255]]);
                assert_eq!(effect.speed, 3);
                // defaults applied
                assert_eq!(effect.brightness, 4);
                assert_eq!(effect.direction, llw_effects::Direction::Forward);
            }
            _ => panic!("expected SetEffect"),
        }

        // Partial effect object — all fields omitted except kind; serde defaults fill the rest.
        let partial = r#"{"v":1,"method":"SetEffect","mac":"02:8b:51:62:32:e1","effect":{"kind":"ripple"}}"#;
        let env: RequestEnvelope = serde_json::from_str(partial).unwrap();
        match env.req {
            Request::SetEffect { effect, .. } => {
                assert_eq!(effect.speed, 3, "partial effect must default speed to 3");
                assert_eq!(effect.brightness, 4, "partial effect must default brightness to 4");
                assert!(effect.colors.is_empty(), "partial effect must default colors to empty");
            }
            _ => panic!("expected SetEffect"),
        }
    }

    #[test]
    fn set_config_envelope_carries_presets_through() {
        // Presets are pass-through data: SetConfig must parse them and the
        // GetConfig handler's `serde_json::to_value(&cfg)` must emit them back.
        let line = r#"{"v":1,"method":"SetConfig","config":{
            "schema_version": 1,
            "presets": [
                {"name":"ocean","effect":{"kind":"ripple","colors":[[0,0,255]],"speed":2,"direction":"reverse","brightness":3}},
                {"name":"plain","effect":{"kind":"breathing"}}
            ]}}"#;
        let env: RequestEnvelope = serde_json::from_str(line).unwrap();
        let Request::SetConfig { config } = env.req else { panic!("expected SetConfig") };
        assert_eq!(config.presets.len(), 2);
        assert_eq!(config.presets[0].name, "ocean");
        assert_eq!(config.presets[0].effect.colors, vec![[0u8, 0, 255]]);
        assert_eq!(config.presets[1].effect.speed, 3, "EffectSpec serde defaults must fill");
        // GetConfig path (same serialization the handler performs)
        let v = serde_json::to_value(&config).unwrap();
        assert_eq!(v["presets"][0]["name"], "ocean");
        assert_eq!(v["presets"][1]["effect"]["speed"], 3);
    }

    #[test]
    fn unknown_method_is_a_parse_error() {
        let line = r#"{"v":1,"method":"Frobnicate"}"#;
        assert!(serde_json::from_str::<RequestEnvelope>(line).is_err());
    }

    #[test]
    fn bind_unbind_envelope_shapes() {
        let line = r#"{"v":1,"method":"Bind","mac":"aa:bb:cc:dd:ee:ff"}"#;
        let env: RequestEnvelope = serde_json::from_str(line).unwrap();
        assert!(matches!(env.req, Request::Bind { .. }));

        let line = r#"{"v":1,"method":"Unbind","mac":"aa:bb:cc:dd:ee:ff"}"#;
        let env: RequestEnvelope = serde_json::from_str(line).unwrap();
        assert!(matches!(env.req, Request::Unbind { .. }));
    }

    #[test]
    fn process_line_version_gate_and_dispatch() {
        let (tx, rx) = std::sync::mpsc::channel::<IpcCmd>();
        // answer thread: reply "pong" to whatever arrives
        std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                let _ = cmd.reply.send(ResponseEnvelope::ok(Some(serde_json::json!("pong"))));
            }
        });
        let resp = process_line(r#"{"v":1,"method":"Ping"}"#, &tx);
        assert!(resp.ok);
        let resp = process_line(r#"{"v":9,"method":"Ping"}"#, &tx);
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("version"));
        let resp = process_line("not json", &tx);
        assert!(!resp.ok);
    }
}
