//! One-shot IPC client for llw-daemon's Unix socket (envelope v1).
//!
//! Mirrors the CLI's proven request path (`crates/llw-cli`): connect to
//! `$XDG_RUNTIME_DIR/llw-daemon.sock`, write one JSON request line, read one
//! reply line. Zero policy — no retries, no validation; the daemon's error
//! strings pass through verbatim (retry UX belongs to the frontend).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// Stable marker prefixing every [`IpcError::Unreachable`] message once
/// stringified for the frontend. The frontend matches on this to distinguish
/// "daemon down" (show the reconnect banner) from a daemon refusal (surface
/// the message verbatim).
pub const UNREACHABLE_PREFIX: &str = "daemon unreachable";

/// Why an IPC request produced no usable result.
#[derive(Debug)]
pub enum IpcError {
    /// Connect/write/read failed, or the reply line was not valid JSON — the
    /// daemon is down or not speaking the protocol. Displays with
    /// [`UNREACHABLE_PREFIX`].
    Unreachable(String),
    /// The daemon replied `ok: false`; the payload is its error string,
    /// verbatim.
    Failed(String),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::Unreachable(detail) => write!(f, "{UNREACHABLE_PREFIX}: {detail}"),
            IpcError::Failed(msg) => f.write_str(msg),
        }
    }
}

impl From<IpcError> for String {
    fn from(e: IpcError) -> Self {
        e.to_string()
    }
}

/// The daemon's socket path — same derivation as llw-daemon and the CLI.
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("llw-daemon.sock")
}

/// Send one envelope-v1 request line to the socket at `path`; return the
/// reply's `data` field (JSON `null` when the daemon sent none).
///
/// The path is a parameter (rather than calling [`socket_path`] internally)
/// so tests can point at a listener on a temp path instead of the real daemon.
pub fn request(path: &Path, req: &serde_json::Value) -> Result<serde_json::Value, IpcError> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| IpcError::Unreachable(format!("connecting {}: {e}", path.display())))?;
    let line = serde_json::to_string(req)
        .map_err(|e| IpcError::Unreachable(format!("encoding request: {e}")))?;
    writeln!(stream, "{line}")
        .map_err(|e| IpcError::Unreachable(format!("writing request: {e}")))?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| IpcError::Unreachable(format!("reading reply: {e}")))?;
    let mut resp: serde_json::Value = serde_json::from_str(&reply)
        .map_err(|e| IpcError::Unreachable(format!("bad reply: {e}")))?;
    if resp["ok"].as_bool().unwrap_or(false) {
        Ok(resp
            .get_mut("data")
            .map(serde_json::Value::take)
            .unwrap_or(serde_json::Value::Null))
    } else {
        Err(IpcError::Failed(
            resp["error"].as_str().unwrap_or("daemon error").to_string(),
        ))
    }
}

/// Test support: bind a listener on a fresh temp socket, serve exactly one
/// connection (read one request line, answer with `reply`), and hand back the
/// captured request as parsed JSON via the join handle. The `TempDir` keeps
/// the socket path alive for the caller's lifetime.
#[cfg(test)]
pub(crate) fn serve_one(
    reply: &str,
) -> (tempfile::TempDir, PathBuf, std::thread::JoinHandle<serde_json::Value>) {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llw-daemon.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let reply = reply.to_string();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        writeln!(stream, "{reply}").unwrap();
        serde_json::from_str(&line).unwrap()
    });
    (dir, path, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn socket_path_targets_the_daemon_socket() {
        assert_eq!(
            socket_path().file_name().and_then(|n| n.to_str()),
            Some("llw-daemon.sock")
        );
    }

    #[test]
    fn unreachable_socket_is_typed_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nobody-home.sock"); // nothing listening
        let err = request(&path, &json!({"v": 1, "method": "Status"})).unwrap_err();
        assert!(matches!(err, IpcError::Unreachable(_)), "got: {err:?}");
        assert!(err.to_string().starts_with(UNREACHABLE_PREFIX));
    }

    #[test]
    fn ok_false_is_typed_failed_with_verbatim_error() {
        let (_dir, path, server) =
            serve_one(r#"{"v":1,"ok":false,"error":"radio settling — try again shortly"}"#);
        let err = request(&path, &json!({"v": 1, "method": "Status"})).unwrap_err();
        match err {
            IpcError::Failed(msg) => assert_eq!(msg, "radio settling — try again shortly"),
            other => panic!("expected Failed, got: {other:?}"),
        }
        server.join().unwrap();
    }

    #[test]
    fn garbled_reply_is_unreachable_not_failed() {
        let (_dir, path, server) = serve_one("this is not json");
        let err = request(&path, &json!({"v": 1, "method": "Status"})).unwrap_err();
        assert!(matches!(err, IpcError::Unreachable(_)), "got: {err:?}");
        server.join().unwrap();
    }

    #[test]
    fn missing_data_field_yields_null() {
        let (_dir, path, server) = serve_one(r#"{"v":1,"ok":true}"#);
        let data = request(&path, &json!({"v": 1, "method": "Ping"})).unwrap();
        assert_eq!(data, serde_json::Value::Null);
        server.join().unwrap();
    }
}
