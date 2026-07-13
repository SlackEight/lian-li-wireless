//! Versioned IPC: newline-delimited JSON over a Unix socket.
//! Envelope carries `v` (protocol version); unknown versions are rejected
//! with a structured error so mismatched daemon/CLI pairs fail actionably.

use crate::config::Config;
use crate::reliability::Telemetry;
use serde::{Deserialize, Serialize};

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
}
