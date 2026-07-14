/// Convert HSV to RGB. `h` ∈ [0, 1) (hue), `s` ∈ [0, 1] (saturation),
/// `v` ∈ [0, 1] (value). Uses the standard six-sextant algorithm.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    if s == 0.0 {
        let c = (v * 255.0).round() as u8;
        return [c, c, c];
    }
    let h = h.rem_euclid(1.0) * 6.0; // sextant index in [0, 6)
    let i = h.floor() as u32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));

    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q), // 5
    };
    [(r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8]
}

/// Linear interpolation between two RGB colours. `x` is clamped to [0, 1].
#[inline]
pub fn lerp(a: [u8; 3], b: [u8; 3], x: f32) -> [u8; 3] {
    let x = x.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * x).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * x).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * x).round() as u8,
    ]
}

/// Scale an RGB colour by a factor `k` ∈ [0, 1].
#[inline]
pub fn scale(c: [u8; 3], k: f32) -> [u8; 3] {
    let k = k.clamp(0.0, 1.0);
    [(c[0] as f32 * k).round() as u8, (c[1] as f32 * k).round() as u8, (c[2] as f32 * k).round() as u8]
}

/// Evaluate a looped piecewise-linear palette at position `x` ∈ [0, 1).
///
/// - Empty slice → white `[255, 255, 255]`.
/// - Single colour → that colour (constant).
/// - Multiple colours: the range [0, 1) is split into `n` equal segments;
///   segment `i` interpolates from `colors[i]` to `colors[(i+1) % n]`.
///   This means the last segment wraps back to `colors[0]`, enabling seamless
///   looping in animations.
pub fn palette(colors: &[[u8; 3]], x: f32) -> [u8; 3] {
    match colors.len() {
        0 => [255, 255, 255],
        1 => colors[0],
        n => {
            let x = x.rem_euclid(1.0);
            let scaled = x * n as f32;
            let i = scaled.floor() as usize;
            let t = scaled - i as f32;
            let a = colors[i % n];
            let b = colors[(i + 1) % n];
            lerp(a, b, t)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_hue_0_is_red() {
        // h=0 → pure red
        let c = hsv_to_rgb(0.0, 1.0, 1.0);
        assert_eq!(c, [255, 0, 0]);
    }

    #[test]
    fn hsv_hue_third_is_green() {
        // h=1/3 (120°) → pure green
        let c = hsv_to_rgb(1.0 / 3.0, 1.0, 1.0);
        assert_eq!(c, [0, 255, 0]);
    }

    #[test]
    fn hsv_hue_two_thirds_is_blue() {
        // h=2/3 (240°) → pure blue
        let c = hsv_to_rgb(2.0 / 3.0, 1.0, 1.0);
        assert_eq!(c, [0, 0, 255]);
    }

    #[test]
    fn hsv_zero_saturation_is_grey() {
        let c = hsv_to_rgb(0.5, 0.0, 0.5);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
    }

    #[test]
    fn palette_empty_is_white() {
        assert_eq!(palette(&[], 0.0), [255, 255, 255]);
        assert_eq!(palette(&[], 0.7), [255, 255, 255]);
    }

    #[test]
    fn palette_single_is_constant() {
        let col = [100u8, 200, 50];
        assert_eq!(palette(&[col], 0.0), col);
        assert_eq!(palette(&[col], 0.99), col);
    }

    #[test]
    fn palette_two_colors_endpoints() {
        let red = [255u8, 0, 0];
        let blue = [0u8, 0, 255];
        let colors = [red, blue];

        // x=0.0 → segment 0, t=0 → red
        assert_eq!(palette(&colors, 0.0), red);
        // x=0.5 → segment 1, t=0 → blue
        assert_eq!(palette(&colors, 0.5), blue);
        // x=0.25 → segment 0, t=0.5 → midpoint of red→blue
        let mid = palette(&colors, 0.25);
        assert_eq!(mid[0], 128); // (255*0.5).round()
        assert_eq!(mid[2], 128); // (255*0.5).round()
    }

    #[test]
    fn palette_wraparound() {
        let red = [255u8, 0, 0];
        let blue = [0u8, 0, 255];
        let colors = [red, blue];

        // x just below 1.0 → last segment, interpolating blue → red (wraparound)
        // At x=0.99: scaled=1.98, i=1, t=0.98. a=blue, b=red.
        let c = palette(&colors, 0.99);
        // Should be much closer to red than blue: r > 200, b < 10
        assert!(c[0] > 200, "red channel should be high near wraparound, got {}", c[0]);
        assert!(c[2] < 10, "blue channel should be low near wraparound, got {}", c[2]);
    }
}
