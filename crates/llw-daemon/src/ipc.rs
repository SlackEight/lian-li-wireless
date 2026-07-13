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
    fn unknown_method_is_a_parse_error() {
        let line = r#"{"v":1,"method":"Frobnicate"}"#;
        assert!(serde_json::from_str::<RequestEnvelope>(line).is_err());
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
