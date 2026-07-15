//! Native-parity golden fixtures for the WASM effects bridge.
//!
//! `generate_goldens` (ignored) writes
//! `crates/llw-ui/ui/src/lib/wasm-goldens/goldens.json` — rendered NATIVELY
//! through the same code path the WASM export uses. The vitest suite
//! (`ui/src/lib/wasm.test.ts`) loads the wasm-pack build and asserts
//! byte-equality against these fixtures, guarding the native↔wasm boundary.
//!
//! Regenerate after changing llw-effects:
//!
//! ```sh
//! cargo test -p llw-effects-wasm generate_goldens -- --ignored
//! ```
//!
//! `goldens_up_to_date` (not ignored) fails the normal test run if the
//! committed fixture drifts from what the current engine renders.

use llw_effects_wasm::render_animation_json_native;

const GOLDENS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../llw-ui/ui/src/lib/wasm-goldens/goldens.json"
);

const SL_INF_3X44: &str =
    r#"{"type":"fans","fan_count":3,"leds_per_fan":44,"layout":"sl_inf44"}"#;
const UNIFORM_2X16: &str =
    r#"{"type":"fans","fan_count":2,"leds_per_fan":16,"layout":"uniform_ring"}"#;

/// (name, spec JSON, geometry JSON, frames).
///
/// Frame counts mirror the hardware budget `clamp(28000/(leds*3), 8, 96)`:
/// 3×44 SL-INF → 70 (a deliberate non-96 sanity case), 2×16 ring → 96.
fn cases() -> Vec<(&'static str, &'static str, &'static str, u16)> {
    vec![
        (
            "ripple-sl_inf44-3x44-70f",
            r#"{"kind":"ripple","colors":[[0,160,255]],"speed":3,"brightness":4}"#,
            SL_INF_3X44,
            70,
        ),
        (
            "rainbow-reverse-sl_inf44-3x44-70f",
            r#"{"kind":"rainbow","speed":5,"direction":"reverse","brightness":3}"#,
            SL_INF_3X44,
            70,
        ),
        (
            "breathing-uniform_ring-2x16-96f",
            r#"{"kind":"breathing","colors":[[255,64,0],[128,0,255]],"speed":1}"#,
            UNIFORM_2X16,
            96,
        ),
    ]
}

#[derive(serde::Deserialize)]
struct RenderOut {
    frames: u16,
    interval_ms: u16,
    leds: usize,
    rgb: Vec<u8>,
}

/// Build the full goldens document as a JSON string (compact + trailing
/// newline). Deterministic: struct field order is fixed and all values are
/// integers.
fn build_goldens_json() -> String {
    #[derive(serde::Serialize)]
    struct Expect {
        frames: u16,
        interval_ms: u16,
        leds: usize,
        first_frame: Vec<u8>,
        middle_index: u16,
        middle_frame: Vec<u8>,
    }
    #[derive(serde::Serialize)]
    struct Case {
        name: &'static str,
        spec: serde_json::Value,
        geometry: serde_json::Value,
        frames: u16,
        expect: Expect,
    }
    #[derive(serde::Serialize)]
    struct Doc {
        #[serde(rename = "_readme")]
        readme: &'static str,
        cases: Vec<Case>,
    }

    let cases = cases()
        .into_iter()
        .map(|(name, spec_json, geometry_json, frames)| {
            let out = render_animation_json_native(spec_json, geometry_json, frames)
                .expect("golden case must render");
            let out: RenderOut = serde_json::from_str(&out).expect("bridge output must parse");
            assert_eq!(out.frames, frames, "{name}: unexpected frame clamp");
            assert_eq!(out.rgb.len(), frames as usize * out.leds * 3, "{name}: rgb size");

            let frame_len = out.leds * 3;
            let middle_index = frames / 2;
            let mid = middle_index as usize * frame_len;
            Case {
                name,
                spec: serde_json::from_str(spec_json).unwrap(),
                geometry: serde_json::from_str(geometry_json).unwrap(),
                frames,
                expect: Expect {
                    frames: out.frames,
                    interval_ms: out.interval_ms,
                    leds: out.leds,
                    first_frame: out.rgb[..frame_len].to_vec(),
                    middle_index,
                    middle_frame: out.rgb[mid..mid + frame_len].to_vec(),
                },
            }
        })
        .collect();

    let doc = Doc {
        readme: "Native-rendered parity fixtures for llw-effects-wasm. \
                 Regenerate: cargo test -p llw-effects-wasm generate_goldens -- --ignored. \
                 Consumed by ui/src/lib/wasm.test.ts (byte-exact vs the wasm-pack build).",
        cases,
    };
    let mut json = serde_json::to_string(&doc).expect("goldens serialise");
    json.push('\n');
    json
}

/// Regenerates the committed fixture. Run explicitly:
/// `cargo test -p llw-effects-wasm generate_goldens -- --ignored`
#[test]
#[ignore = "writes the committed goldens fixture; run explicitly to regenerate"]
fn generate_goldens() {
    let path = std::path::Path::new(GOLDENS_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).expect("create wasm-goldens dir");
    std::fs::write(path, build_goldens_json()).expect("write goldens.json");
    eprintln!("wrote {}", path.display());
}

/// Fails when the committed fixture no longer matches the current engine —
/// regenerate (see above) and commit the diff alongside the effects change.
#[test]
fn goldens_up_to_date() {
    let committed = std::fs::read_to_string(GOLDENS_PATH).unwrap_or_else(|e| {
        panic!(
            "missing goldens fixture at {GOLDENS_PATH} ({e}); \
             run `cargo test -p llw-effects-wasm generate_goldens -- --ignored`"
        )
    });
    assert_eq!(
        committed,
        build_goldens_json(),
        "goldens.json is stale — regenerate with \
         `cargo test -p llw-effects-wasm generate_goldens -- --ignored` and commit it"
    );
}
