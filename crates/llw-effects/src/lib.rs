//! `llw-effects` — pure, deterministic RGB effect engine.
//!
//! Effects are pure functions `(spec, geometry, t) → frame` where `t ∈ [0,1)`
//! is the phase of one animation period. The daemon compiles a spec into F
//! frames at `t = i/F` and uploads once; the firmware loops it.

pub mod color;
pub mod geometry;
mod effects;

pub use geometry::Geometry;

// ---------------------------------------------------------------------------
// EffectKind
// ---------------------------------------------------------------------------

/// The eight named effects in the v1 catalog.
///
/// Serde uses kebab-case names (`"color-cycle"`, `"rainbow-morph"`, etc.)
/// for clarity over bare lowercase identifiers.
///
/// `static` is a Rust keyword; the variant is named `Static` and serialises
/// as `"static"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectKind {
    /// Every LED shows a fixed colour (default white).
    #[serde(rename = "static")]
    Static,
    /// Whole device breathes — brightness follows sin²(πt).
    Breathing,
    /// Smooth palette fade across the whole device over one period.
    ColorCycle,
    /// Whole device cycles through the hue wheel together.
    RainbowMorph,
    /// Moving rainbow tied to ring/strip position.
    Rainbow,
    /// A bright head trails an exponential fade across the chain.
    Meteor,
    /// Marching on/off segments.
    Runway,
    /// A pulse expands from the origin outward.
    Ripple,
}

impl EffectKind {
    /// All variants in definition order — used by the CLI catalog command.
    pub fn all() -> &'static [EffectKind] {
        &[
            EffectKind::Static,
            EffectKind::Breathing,
            EffectKind::ColorCycle,
            EffectKind::RainbowMorph,
            EffectKind::Rainbow,
            EffectKind::Meteor,
            EffectKind::Runway,
            EffectKind::Ripple,
        ]
    }

    /// One-line description for the CLI `llw effects` listing.
    pub fn describe(&self) -> &'static str {
        match self {
            EffectKind::Static      => "Fixed colour (default white)",
            EffectKind::Breathing   => "Smooth brightness pulse, one breath per period",
            EffectKind::ColorCycle  => "Whole device fades through the palette",
            EffectKind::RainbowMorph => "Whole device cycles through the hue wheel",
            EffectKind::Rainbow     => "Moving rainbow across ring/strip position",
            EffectKind::Meteor      => "Bright head with exponential trailing fade",
            EffectKind::Runway      => "Marching on/off segments",
            EffectKind::Ripple      => "Expanding pulse from origin, fades at the edge",
        }
    }

    /// `true` for effects where direction (Forward/Reverse) changes the visual.
    ///
    /// `render_animation` maps `t → 1−t` for Reverse only when this is true.
    /// Uniform effects (Static, Breathing, ColorCycle, RainbowMorph) are
    /// direction-invariant so the flag is false for them.
    pub fn directional(&self) -> bool {
        matches!(self, EffectKind::Rainbow | EffectKind::Meteor | EffectKind::Runway)
    }
}

// ---------------------------------------------------------------------------
// Direction
// ---------------------------------------------------------------------------

/// Animation direction. `Reverse` mirrors the time axis for directional
/// effects (`t → 1 − t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Forward,
    Reverse,
}

// ---------------------------------------------------------------------------
// EffectSpec
// ---------------------------------------------------------------------------

/// Complete specification for a single animated effect.
///
/// All fields carry serde defaults so partial JSON (e.g. `{"kind":"ripple"}`)
/// deserialises cleanly. The daemon/IPC treat missing fields as "use default".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EffectSpec {
    /// Which effect algorithm to run.
    pub kind: EffectKind,

    /// Colour palette. Interpretation varies by effect; empty → effect default
    /// (usually white). Maximum 8 entries enforced by the daemon validator.
    #[serde(default)]
    pub colors: Vec<[u8; 3]>,

    /// Animation speed 1..=5 (clamped). Maps to a period via [`period_ms`].
    /// Default 3 (medium — 3 000 ms period).
    #[serde(default = "default_speed")]
    pub speed: u8,

    /// Animation direction. Only applied to directional effects.
    /// Default `Forward`.
    #[serde(default)]
    pub direction: Direction,

    /// Brightness 0..=4. Applied as a `×(brightness/4)` post-render scale.
    /// Default 4 (full brightness).
    #[serde(default = "default_brightness")]
    pub brightness: u8,
}

fn default_speed() -> u8 { 3 }
fn default_brightness() -> u8 { 4 }

// ---------------------------------------------------------------------------
// Speed → period map (LOCKED for v1)
// ---------------------------------------------------------------------------

/// Map speed (1..=5, clamped) to animation period in milliseconds.
///
/// | Speed | Period |
/// |-------|--------|
/// | 1     | 6 000 ms |
/// | 2     | 4 200 ms |
/// | 3     | 3 000 ms |
/// | 4     | 2 100 ms |
/// | 5     | 1 400 ms |
pub fn period_ms(speed: u8) -> u32 {
    const TABLE: [u32; 5] = [6_000, 4_200, 3_000, 2_100, 1_400];
    let idx = speed.clamp(1, 5) as usize - 1;
    TABLE[idx]
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render a single frame at phase `t ∈ [0, 1)`.
///
/// `t` must already be direction-adjusted by the caller (`render_animation`
/// does this). Brightness scaling (×`brightness/4`) is applied here after the
/// effect produces its raw frame.
pub fn render_frame(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    let raw = effects::dispatch(spec, geom, t);
    let k = (spec.brightness as f32 / 4.0).clamp(0.0, 1.0);
    raw.into_iter().map(|c| color::scale(c, k)).collect()
}

/// Render a full animation and return `(frames, interval_ms)`.
///
/// `frames` is clamped to at least 1. `interval_ms = period_ms(speed) / frames`,
/// clamped to ≥ 20 ms. For directional effects (`EffectKind::directional()`)
/// `Reverse` maps `t → 1 − t`.
pub fn render_animation(
    spec: &EffectSpec,
    geom: &Geometry,
    frames: u16,
) -> (Vec<Vec<[u8; 3]>>, u16) {
    let frames = frames.max(1);
    let period = period_ms(spec.speed);
    let interval_ms = ((period / frames as u32).max(20)) as u16;

    let reverse = spec.direction == Direction::Reverse && spec.kind.directional();

    let rendered = (0..frames)
        .map(|i| {
            let mut t = i as f32 / frames as f32;
            if reverse {
                t = 1.0 - t;
            }
            render_frame(spec, geom, t)
        })
        .collect();

    (rendered, interval_ms)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn base_spec(kind: EffectKind) -> EffectSpec {
        EffectSpec {
            kind,
            colors: vec![],
            speed: 3,
            direction: Direction::Forward,
            brightness: 4,
        }
    }

    // --- period_ms ---

    #[test]
    fn period_table() {
        assert_eq!(period_ms(1), 6_000);
        assert_eq!(period_ms(2), 4_200);
        assert_eq!(period_ms(3), 3_000);
        assert_eq!(period_ms(4), 2_100);
        assert_eq!(period_ms(5), 1_400);
    }

    #[test]
    fn period_clamped_low() {
        assert_eq!(period_ms(0), period_ms(1));
    }

    #[test]
    fn period_clamped_high() {
        assert_eq!(period_ms(6), period_ms(5));
    }

    // --- brightness 0 → all black ---

    #[test]
    fn brightness_zero_all_black() {
        let mut spec = base_spec(EffectKind::Static);
        spec.brightness = 0;
        let geom = Geometry::Fans { fan_count: 3, leds_per_fan: 44 };
        let frame = render_frame(&spec, &geom, 0.0);
        assert!(
            frame.iter().all(|&c| c == [0, 0, 0]),
            "brightness 0 must produce all-black frame"
        );
    }

    // --- determinism ---

    #[test]
    fn render_frame_deterministic() {
        let spec = base_spec(EffectKind::Static);
        let geom = Geometry::Fans { fan_count: 3, leds_per_fan: 44 };
        let a = render_frame(&spec, &geom, 0.42);
        let b = render_frame(&spec, &geom, 0.42);
        assert_eq!(a, b, "render_frame must be deterministic");
    }

    // --- interval math ---

    #[test]
    fn interval_24_frames_speed3() {
        // period_ms(3) = 3000; 3000 / 24 = 125 ms
        let spec = base_spec(EffectKind::Static);
        let geom = Geometry::Strip { total: 10 };
        let (frames, interval) = render_animation(&spec, &geom, 24);
        assert_eq!(frames.len(), 24);
        assert_eq!(interval, 125, "interval should be 125 ms for 24 frames @ speed 3");
    }

    #[test]
    fn interval_clamped_to_20ms() {
        // Very high frame count should clamp interval to 20 ms.
        let mut spec = base_spec(EffectKind::Static);
        spec.speed = 5; // period = 1400 ms; 1400/200 = 7 ms → clamp to 20
        let geom = Geometry::Strip { total: 5 };
        let (_frames, interval) = render_animation(&spec, &geom, 200);
        assert_eq!(interval, 20, "interval must not go below 20 ms");
    }

    #[test]
    fn frames_minimum_one() {
        let spec = base_spec(EffectKind::Static);
        let geom = Geometry::Strip { total: 5 };
        let (frames, _) = render_animation(&spec, &geom, 0);
        assert_eq!(frames.len(), 1, "render_animation must produce at least 1 frame");
    }

    // --- Static via render_animation ---

    #[test]
    fn static_animation_all_white() {
        let spec = base_spec(EffectKind::Static);
        let geom = Geometry::Fans { fan_count: 3, leds_per_fan: 44 };
        let (frames, _) = render_animation(&spec, &geom, 4);
        for frame in &frames {
            assert!(frame.iter().all(|&c| c == [255, 255, 255]));
        }
    }

    // --- serde defaults ---

    #[test]
    fn spec_serde_partial_json() {
        let json = r#"{"kind":"static"}"#;
        let spec: EffectSpec = serde_json::from_str(json).expect("partial JSON should deserialise");
        assert_eq!(spec.speed, 3);
        assert_eq!(spec.brightness, 4);
        assert_eq!(spec.direction, Direction::Forward);
        assert!(spec.colors.is_empty());
    }

    #[test]
    fn effect_kind_serde_roundtrip() {
        let kinds = EffectKind::all();
        for kind in kinds {
            let json = serde_json::to_string(kind).unwrap();
            let back: EffectKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back, "round-trip failed for {:?}", kind);
        }
    }

    #[test]
    fn effect_kind_static_serde_name() {
        let json = serde_json::to_string(&EffectKind::Static).unwrap();
        assert_eq!(json, r#""static""#);
    }

    // --- wire-name pins (carry-forward from Task 1 review) ---

    #[test]
    fn wire_name_color_cycle() {
        // EffectKind::ColorCycle must serialise to "color-cycle" (kebab-case).
        let json = serde_json::to_string(&EffectKind::ColorCycle).unwrap();
        assert_eq!(json, r#""color-cycle""#, "ColorCycle wire name mismatch");
    }

    #[test]
    fn wire_name_rainbow_morph() {
        // EffectKind::RainbowMorph must serialise to "rainbow-morph".
        let json = serde_json::to_string(&EffectKind::RainbowMorph).unwrap();
        assert_eq!(json, r#""rainbow-morph""#, "RainbowMorph wire name mismatch");
    }

    #[test]
    fn partial_json_ripple_uses_defaults() {
        // {"kind":"ripple"} must deserialise cleanly with all defaults applied.
        let json = r#"{"kind":"ripple"}"#;
        let spec: EffectSpec = serde_json::from_str(json)
            .expect(r#"{"kind":"ripple"} should deserialise with defaults"#);
        assert_eq!(spec.kind, EffectKind::Ripple);
        assert_eq!(spec.speed, 3);
        assert_eq!(spec.brightness, 4);
        assert_eq!(spec.direction, Direction::Forward);
        assert!(spec.colors.is_empty());
    }

    #[test]
    fn unknown_kind_errors() {
        // An unrecognised kind must produce a deserialisation error, not silently default.
        let json = r#"{"kind":"frobnicate"}"#;
        let result: Result<EffectSpec, _> = serde_json::from_str(json);
        assert!(result.is_err(), "unknown kind should fail to deserialise");
    }

    // --- directional / direction ---

    #[test]
    fn directional_flags() {
        assert!(EffectKind::Rainbow.directional());
        assert!(EffectKind::Meteor.directional());
        assert!(EffectKind::Runway.directional());
        assert!(!EffectKind::Static.directional());
        assert!(!EffectKind::Breathing.directional());
        assert!(!EffectKind::Ripple.directional());
    }
}
