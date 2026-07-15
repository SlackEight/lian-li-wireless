//! `llw-effects-wasm` — thin wasm-bindgen bridge over [`llw_effects`] for the
//! UI preview canvas.
//!
//! The boundary is cold (one call per spec/device change), so the API is
//! deliberately tiny and stringly: JSON in, JSON out. All real logic lives in
//! `llw-effects`; this crate only parses, calls, and serialises.
//!
//! The core functions (`*_impl`) are plain Rust returning `Result<_, String>`
//! so native tests and the parity-golden generator exercise the exact same
//! code path the WASM exports use; the `#[wasm_bindgen]` wrappers only map
//! errors to `JsValue`.

use llw_effects::geometry::{led_polar, Geometry};
use llw_effects::EffectSpec;
use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// render_animation_json
// ---------------------------------------------------------------------------

/// JSON shape returned by [`render_animation_json`].
#[derive(serde::Serialize)]
struct RenderResult {
    /// Number of frames actually rendered (input clamped to ≥ 1).
    frames: u16,
    /// Per-frame interval in milliseconds — the second element of the
    /// `llw_effects::render_animation` tuple: `period_ms(speed) / frames`,
    /// clamped to ≥ 20 ms. This is the playback cadence the hardware uses.
    interval_ms: u16,
    /// Total LED count (`geometry.len()`).
    leds: usize,
    /// Flat frame-major RGB bytes: `frames × leds × 3` numbers, i.e.
    /// `rgb[(frame*leds + led)*3 + channel]`. Plain JSON numbers — no base64.
    rgb: Vec<u8>,
}

fn render_animation_json_impl(
    spec_json: &str,
    geometry_json: &str,
    frames: u16,
) -> Result<String, String> {
    let spec: EffectSpec =
        serde_json::from_str(spec_json).map_err(|e| format!("invalid effect spec: {e}"))?;
    let geom: Geometry =
        serde_json::from_str(geometry_json).map_err(|e| format!("invalid geometry: {e}"))?;

    let (rendered, interval_ms) = llw_effects::render_animation(&spec, &geom, frames);

    let frames_rendered = rendered.len() as u16;
    let leds = geom.len();
    let mut rgb = Vec::with_capacity(rendered.len() * leds * 3);
    for frame in &rendered {
        for led in frame {
            rgb.extend_from_slice(led);
        }
    }

    let result = RenderResult { frames: frames_rendered, interval_ms, leds, rgb };
    serde_json::to_string(&result).map_err(|e| format!("serialise render result: {e}"))
}

/// Render a full animation from JSON inputs.
///
/// - `spec_json`: an `EffectSpec` (serde defaults apply — `{"kind":"ripple"}`
///   is valid).
/// - `geometry_json`: a `Geometry` (e.g.
///   `{"type":"fans","fan_count":3,"leds_per_fan":44,"layout":"sl_inf44"}`).
/// - `frames`: frame count; the UI passes the hardware budget
///   `clamp(28000/(leds*3), 8, 96)`. Clamped to ≥ 1 like the native engine.
///
/// Returns JSON `{"frames", "interval_ms", "leds", "rgb"}` (see
/// [`RenderResult`]). Parse errors are returned as `Err(String)` with the
/// serde message so the UI can toast it verbatim.
#[wasm_bindgen]
pub fn render_animation_json(
    spec_json: &str,
    geometry_json: &str,
    frames: u16,
) -> Result<String, JsValue> {
    render_animation_json_impl(spec_json, geometry_json, frames)
        .map_err(|e| JsValue::from_str(&e))
}

// ---------------------------------------------------------------------------
// led_layout_json
// ---------------------------------------------------------------------------

/// One entry per LED in wire order (fan-major), returned by
/// [`led_layout_json`].
#[derive(serde::Serialize)]
struct LedPoint {
    /// Fan index (0-based) so the UI can lay out one cluster per fan.
    /// Always 0 for `Strip` geometry.
    fan: u8,
    x: f32,
    y: f32,
}

/// Coordinate convention (documented for the canvas consumer):
///
/// `led_polar` angles are fractional turns with **0 = top, increasing
/// clockwise** (see `geometry.rs`). We emit *y-up* unit coordinates:
///
/// ```text
/// x = sin(angle·2π) · r,   y = cos(angle·2π) · r
/// ```
///
/// so angle 0 → (0, 1) top, 0.25 → (1, 0) right, 0.5 → (0, −1) bottom,
/// 0.75 → (−1, 0) left — i.e. the SL-INF inner ring (angle 0.75 at index 0,
/// "clockwise from left-middle") starts at left-middle and winds clockwise
/// as seen on screen. Canvas consumers must flip y when drawing
/// (`screenY = cy − y·scale`) because canvas y grows downward.
///
/// Radii are normalised by the layout's maximum radius (UniformRing → 1.0,
/// SlInf44 → 1.15) so every point fits inside the unit circle.
fn led_layout_json_impl(geometry_json: &str) -> Result<String, String> {
    let geom: Geometry =
        serde_json::from_str(geometry_json).map_err(|e| format!("invalid geometry: {e}"))?;

    let points: Vec<LedPoint> = match geom {
        Geometry::Fans { fan_count, leds_per_fan, layout } => {
            // Normalise by the fan's max radius so coordinates fit in [-1, 1].
            let r_max = (0..leds_per_fan)
                .map(|i| led_polar(layout, i, leds_per_fan).1)
                .fold(1.0_f32, f32::max);
            let tau = 2.0 * std::f32::consts::PI;
            (0..fan_count)
                .flat_map(|fan| {
                    (0..leds_per_fan).map(move |i| {
                        let (angle, radius) = led_polar(layout, i, leds_per_fan);
                        let r = radius / r_max;
                        LedPoint {
                            fan,
                            x: (angle * tau).sin() * r,
                            y: (angle * tau).cos() * r,
                        }
                    })
                })
                .collect()
        }
        // Strip: a horizontal line, left → right, centred on y = 0.
        Geometry::Strip { total } => (0..total)
            .map(|i| LedPoint {
                fan: 0,
                x: if total > 1 { 2.0 * i as f32 / (total - 1) as f32 - 1.0 } else { 0.0 },
                y: 0.0,
            })
            .collect(),
    };

    serde_json::to_string(&points).map_err(|e| format!("serialise layout: {e}"))
}

/// Per-LED unit-circle positions for the preview canvas: JSON
/// `[{"fan", "x", "y"}, …]` in wire order. See [`led_layout_json_impl`] for
/// the coordinate convention (y-up, angle 0 = top, clockwise).
#[wasm_bindgen]
pub fn led_layout_json(geometry_json: &str) -> Result<String, JsValue> {
    led_layout_json_impl(geometry_json).map_err(|e| JsValue::from_str(&e))
}

// ---------------------------------------------------------------------------
// Native-testable internals (goldens generator lives in tests/goldens.rs)
// ---------------------------------------------------------------------------

/// Native (non-wasm) access to the same code path the WASM export uses.
/// Public so `tests/goldens.rs` can generate/verify parity fixtures.
pub fn render_animation_json_native(
    spec_json: &str,
    geometry_json: &str,
    frames: u16,
) -> Result<String, String> {
    render_animation_json_impl(spec_json, geometry_json, frames)
}

/// Native (non-wasm) access to [`led_layout_json`]'s code path.
pub fn led_layout_json_native(geometry_json: &str) -> Result<String, String> {
    led_layout_json_impl(geometry_json)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SL_INF_3X44: &str =
        r#"{"type":"fans","fan_count":3,"leds_per_fan":44,"layout":"sl_inf44"}"#;

    #[derive(serde::Deserialize)]
    struct RenderOut {
        frames: u16,
        interval_ms: u16,
        leds: usize,
        rgb: Vec<u8>,
    }

    #[derive(serde::Deserialize)]
    struct PointOut {
        fan: u8,
        x: f32,
        y: f32,
    }

    // --- render_animation_json ---

    #[test]
    fn render_shape_and_interval() {
        // speed 3 → period 3000 ms; 3000/70 = 42 ms (integer division).
        let out = render_animation_json_impl(r#"{"kind":"ripple"}"#, SL_INF_3X44, 70).unwrap();
        let out: RenderOut = serde_json::from_str(&out).unwrap();
        assert_eq!(out.frames, 70);
        assert_eq!(out.interval_ms, 42);
        assert_eq!(out.leds, 132);
        assert_eq!(out.rgb.len(), 70 * 132 * 3);
    }

    #[test]
    fn render_frames_clamped_to_one() {
        let out = render_animation_json_impl(r#"{"kind":"static"}"#, SL_INF_3X44, 0).unwrap();
        let out: RenderOut = serde_json::from_str(&out).unwrap();
        assert_eq!(out.frames, 1, "frames=0 must clamp to 1 like the native engine");
        assert_eq!(out.rgb.len(), 132 * 3);
    }

    #[test]
    fn render_matches_native_engine_bytes() {
        // The bridge must be byte-identical to calling llw_effects directly.
        let spec: EffectSpec = serde_json::from_str(r#"{"kind":"rainbow","speed":5}"#).unwrap();
        let geom: Geometry = serde_json::from_str(SL_INF_3X44).unwrap();
        let (frames, interval) = llw_effects::render_animation(&spec, &geom, 8);

        let out =
            render_animation_json_impl(r#"{"kind":"rainbow","speed":5}"#, SL_INF_3X44, 8).unwrap();
        let out: RenderOut = serde_json::from_str(&out).unwrap();
        assert_eq!(out.interval_ms, interval);
        let flat: Vec<u8> =
            frames.iter().flat_map(|f| f.iter().flat_map(|c| c.iter().copied())).collect();
        assert_eq!(out.rgb, flat);
    }

    #[test]
    fn render_bad_spec_is_err_with_serde_message() {
        let err = render_animation_json_impl(r#"{"kind":"frobnicate"}"#, SL_INF_3X44, 8)
            .unwrap_err();
        assert!(err.starts_with("invalid effect spec:"), "got: {err}");
        assert!(err.contains("frobnicate"), "serde message should name the bad kind: {err}");
    }

    #[test]
    fn render_bad_geometry_is_err() {
        let err = render_animation_json_impl(r#"{"kind":"static"}"#, r#"{"type":"blob"}"#, 8)
            .unwrap_err();
        assert!(err.starts_with("invalid geometry:"), "got: {err}");
    }

    // --- led_layout_json ---

    #[test]
    fn layout_uniform_ring_cardinal_points() {
        // 1 fan × 4 LEDs: angles 0, 0.25, 0.5, 0.75 → top, right, bottom, left (y-up).
        let json = r#"{"type":"fans","fan_count":1,"leds_per_fan":4,"layout":"uniform_ring"}"#;
        let pts: Vec<PointOut> = serde_json::from_str(&led_layout_json_impl(json).unwrap()).unwrap();
        assert_eq!(pts.len(), 4);
        let expect = [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)];
        for (p, (ex, ey)) in pts.iter().zip(expect) {
            assert!((p.x - ex).abs() < 1e-5, "x: got {}, want {ex}", p.x);
            assert!((p.y - ey).abs() < 1e-5, "y: got {}, want {ey}", p.y);
        }
    }

    #[test]
    fn layout_sl_inf44_led0_left_middle() {
        // SlInf44 index 0: angle 0.75, radius 0.7 → left-middle at r = 0.7/1.15.
        let pts: Vec<PointOut> =
            serde_json::from_str(&led_layout_json_impl(SL_INF_3X44).unwrap()).unwrap();
        assert_eq!(pts.len(), 132);
        let r = 0.7_f32 / 1.15;
        assert!((pts[0].x + r).abs() < 1e-5, "LED 0 x must be −{r}, got {}", pts[0].x);
        assert!(pts[0].y.abs() < 1e-5, "LED 0 y must be 0, got {}", pts[0].y);
        // Winding check: LED 1 sits clockwise of LED 0 → upper-left quadrant.
        assert!(pts[1].x < 0.0 && pts[1].y > 0.0, "LED 1 must be upper-left (clockwise)");
    }

    #[test]
    fn layout_fan_indices() {
        let pts: Vec<PointOut> =
            serde_json::from_str(&led_layout_json_impl(SL_INF_3X44).unwrap()).unwrap();
        assert_eq!(pts[0].fan, 0);
        assert_eq!(pts[43].fan, 0);
        assert_eq!(pts[44].fan, 1);
        assert_eq!(pts[131].fan, 2);
        // Same fan → same local positions.
        assert!((pts[0].x - pts[44].x).abs() < 1e-6);
        assert!((pts[0].y - pts[44].y).abs() < 1e-6);
    }

    #[test]
    fn layout_strip_is_horizontal_line() {
        let pts: Vec<PointOut> =
            serde_json::from_str(&led_layout_json_impl(r#"{"type":"strip","total":5}"#).unwrap())
                .unwrap();
        assert_eq!(pts.len(), 5);
        assert!((pts[0].x + 1.0).abs() < 1e-6);
        assert!((pts[4].x - 1.0).abs() < 1e-6);
        assert!((pts[2].x).abs() < 1e-6);
        assert!(pts.iter().all(|p| p.y == 0.0 && p.fan == 0));
    }

    #[test]
    fn layout_bad_geometry_is_err() {
        let err = led_layout_json_impl("not json").unwrap_err();
        assert!(err.starts_with("invalid geometry:"), "got: {err}");
    }
}
