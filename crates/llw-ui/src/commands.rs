//! Tauri commands: thin wrappers over the one-shot IPC client.
//!
//! One daemon request per invocation, zero policy — no retries, no
//! validation. Daemon error strings reach the frontend verbatim; an
//! unreachable socket surfaces as a string carrying
//! [`crate::ipc::UNREACHABLE_PREFIX`] so the frontend can show "daemon down".
//!
//! Each command delegates to a `*_at(path, ..)` twin that takes the socket
//! path explicitly; tests exercise the twins against an in-test listener.

use crate::ipc;
use serde_json::{json, Value};
use std::path::Path;

#[tauri::command]
pub fn status() -> Result<Value, String> {
    status_at(&ipc::socket_path())
}

fn status_at(path: &Path) -> Result<Value, String> {
    ipc::request(path, &json!({"v": 1, "method": "Status"})).map_err(Into::into)
}

#[tauri::command]
pub fn bind(mac: String) -> Result<Value, String> {
    bind_at(&ipc::socket_path(), &mac)
}

fn bind_at(path: &Path, mac: &str) -> Result<Value, String> {
    // Lowercased like the CLI's bind_ipc — the daemon stores MACs lowercase.
    let req = json!({"v": 1, "method": "Bind", "mac": mac.to_lowercase()});
    ipc::request(path, &req).map_err(Into::into)
}

#[tauri::command]
pub fn unbind(mac: String) -> Result<Value, String> {
    unbind_at(&ipc::socket_path(), &mac)
}

fn unbind_at(path: &Path, mac: &str) -> Result<Value, String> {
    let req = json!({"v": 1, "method": "Unbind", "mac": mac.to_lowercase()});
    ipc::request(path, &req).map_err(Into::into)
}

#[tauri::command]
pub fn set_effect(mac: String, spec: Value) -> Result<Value, String> {
    set_effect_at(&ipc::socket_path(), &mac, spec)
}

fn set_effect_at(path: &Path, mac: &str, spec: Value) -> Result<Value, String> {
    // `spec` passes through verbatim; the daemon deserializes EffectSpec
    // (kebab-case kind, serde defaults for omitted fields) and validates.
    let req = json!({"v": 1, "method": "SetEffect", "mac": mac, "effect": spec});
    ipc::request(path, &req).map_err(Into::into)
}

#[tauri::command]
pub fn set_color(mac: String, rgb: [u8; 3], brightness: u8) -> Result<Value, String> {
    set_color_at(&ipc::socket_path(), &mac, rgb, brightness)
}

fn set_color_at(path: &Path, mac: &str, rgb: [u8; 3], brightness: u8) -> Result<Value, String> {
    let req =
        json!({"v": 1, "method": "SetColor", "mac": mac, "rgb": rgb, "brightness": brightness});
    ipc::request(path, &req).map_err(Into::into)
}

#[tauri::command]
pub fn get_config() -> Result<Value, String> {
    get_config_at(&ipc::socket_path())
}

fn get_config_at(path: &Path) -> Result<Value, String> {
    ipc::request(path, &json!({"v": 1, "method": "GetConfig"})).map_err(Into::into)
}

#[tauri::command]
pub fn set_config(json: Value) -> Result<Value, String> {
    set_config_at(&ipc::socket_path(), json)
}

fn set_config_at(path: &Path, config: Value) -> Result<Value, String> {
    // `config` passes through verbatim; the daemon deserializes + validates.
    let req = json!({"v": 1, "method": "SetConfig", "config": config});
    ipc::request(path, &req).map_err(Into::into)
}

#[tauri::command]
pub fn list_sensors() -> Result<Value, String> {
    list_sensors_at(&ipc::socket_path())
}

fn list_sensors_at(path: &Path) -> Result<Value, String> {
    ipc::request(path, &json!({"v": 1, "method": "ListSensors"})).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::serve_one;

    const OK_EMPTY: &str = r#"{"v":1,"ok":true}"#;

    #[test]
    fn status_round_trip() {
        let (_dir, path, server) = serve_one(
            r#"{"v":1,"ok":true,"data":{"daemon_version":"0.1.0","tx_wedged":false,"devices":[]}}"#,
        );
        let data = status_at(&path).unwrap();
        assert_eq!(server.join().unwrap(), json!({"v": 1, "method": "Status"}));
        assert_eq!(data["daemon_version"], "0.1.0");
        assert_eq!(data["tx_wedged"], false);
    }

    #[test]
    fn bind_round_trip_lowercases_mac() {
        let (_dir, path, server) = serve_one(r#"{"v":1,"ok":true,"data":{"state":"started"}}"#);
        let data = bind_at(&path, "AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(
            server.join().unwrap(),
            json!({"v": 1, "method": "Bind", "mac": "aa:bb:cc:dd:ee:ff"})
        );
        assert_eq!(data, json!({"state": "started"}));
    }

    #[test]
    fn unbind_round_trip() {
        let (_dir, path, server) = serve_one(r#"{"v":1,"ok":true,"data":{"state":"started"}}"#);
        let data = unbind_at(&path, "02:8B:51:62:32:E1").unwrap();
        assert_eq!(
            server.join().unwrap(),
            json!({"v": 1, "method": "Unbind", "mac": "02:8b:51:62:32:e1"})
        );
        assert_eq!(data, json!({"state": "started"}));
    }

    #[test]
    fn set_effect_round_trip_passes_spec_verbatim() {
        let (_dir, path, server) = serve_one(OK_EMPTY);
        let spec = json!({"kind": "ripple", "colors": [[0, 0, 255]], "speed": 3});
        let data = set_effect_at(&path, "02:8b:51:62:32:e1", spec.clone()).unwrap();
        assert_eq!(
            server.join().unwrap(),
            json!({"v": 1, "method": "SetEffect", "mac": "02:8b:51:62:32:e1", "effect": spec})
        );
        assert_eq!(data, Value::Null);
    }

    #[test]
    fn set_color_round_trip() {
        let (_dir, path, server) = serve_one(OK_EMPTY);
        let data = set_color_at(&path, "02:8b:51:62:32:e1", [255, 0, 0], 4).unwrap();
        assert_eq!(
            server.join().unwrap(),
            json!({
                "v": 1,
                "method": "SetColor",
                "mac": "02:8b:51:62:32:e1",
                "rgb": [255, 0, 0],
                "brightness": 4
            })
        );
        assert_eq!(data, Value::Null);
    }

    #[test]
    fn get_config_round_trip() {
        let (_dir, path, server) = serve_one(
            r#"{"v":1,"ok":true,"data":{"schema_version":1,"curves":[],"devices":[]}}"#,
        );
        let data = get_config_at(&path).unwrap();
        assert_eq!(server.join().unwrap(), json!({"v": 1, "method": "GetConfig"}));
        assert_eq!(data["schema_version"], 1);
    }

    #[test]
    fn set_config_round_trip_wraps_json_in_config_field() {
        let (_dir, path, server) = serve_one(OK_EMPTY);
        let cfg = json!({"schema_version": 1, "devices": [{"mac": "02:8b:51:62:32:e1"}]});
        let data = set_config_at(&path, cfg.clone()).unwrap();
        assert_eq!(
            server.join().unwrap(),
            json!({"v": 1, "method": "SetConfig", "config": cfg})
        );
        assert_eq!(data, Value::Null);
    }

    #[test]
    fn list_sensors_round_trip() {
        // Reply shape mirrors llw-daemon's ListSensorsData: sensors[].spec is
        // a verbatim config SensorSpec; current_c is null on read failure.
        let (_dir, path, server) = serve_one(
            r#"{"v":1,"ok":true,"data":{"sensors":[{"chip":"k10temp","label":"Tctl","spec":{"hwmon_name":"k10temp","input":"temp1_input"},"current_c":41.25},{"chip":"nvme","label":"Composite","spec":{"hwmon_name":"nvme","input":"temp1_input"},"current_c":null}]}}"#,
        );
        let data = list_sensors_at(&path).unwrap();
        assert_eq!(
            server.join().unwrap(),
            json!({"v": 1, "method": "ListSensors"})
        );
        assert_eq!(data["sensors"][0]["chip"], "k10temp");
        assert_eq!(data["sensors"][0]["label"], "Tctl");
        assert_eq!(data["sensors"][0]["spec"]["hwmon_name"], "k10temp");
        assert_eq!(data["sensors"][0]["spec"]["input"], "temp1_input");
        assert_eq!(data["sensors"][0]["current_c"], 41.25);
        assert!(data["sensors"][1]["current_c"].is_null());
    }

    #[test]
    fn daemon_refusal_string_passes_through_verbatim() {
        let (_dir, path, server) = serve_one(
            r#"{"v":1,"ok":false,"error":"refusing to bind: device is bound to another master"}"#,
        );
        let err = bind_at(&path, "aa:bb:cc:dd:ee:ff").unwrap_err();
        assert_eq!(err, "refusing to bind: device is bound to another master");
        server.join().unwrap();
    }

    #[test]
    fn unreachable_socket_yields_prefixed_error_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-daemon-here.sock"); // nothing listening
        let err = status_at(&path).unwrap_err();
        assert!(
            err.starts_with(crate::ipc::UNREACHABLE_PREFIX),
            "expected the unreachable prefix, got: {err}"
        );
    }
}
