/**
 * Curve editor model — framework-free, mirrors the daemon's curve semantics.
 *
 * Wire shape: llw-daemon's `Curve.points` is `Vec<(f32, f32)>`
 * (crates/llw-daemon/src/config.rs), which serde serializes as
 * `[[temp_c, duty_pct], ...]` — so a point here is the tuple
 * `[temp °C, duty %]`, exactly the JSON that GetConfig/SetConfig round-trip.
 * Stored order is irrelevant to the daemon (it sorts on load); the edit ops
 * below still always return sorted arrays so the editor renders sanely.
 *
 * Units: this model speaks duty PERCENT — that is what config.json stores.
 * The daemon converts % → PWM bytes only at the wire (curve.rs
 * `percent_to_pwm`); `percentToPwm` mirrors that cast for parity tests and
 * for any display that wants the raw byte.
 *
 * Interpolation mirrors curve.rs `SortedCurve::eval` EXACTLY:
 *   - empty curve → 50; single point → its duty
 *   - temp ≤ first point's temp → first duty; temp ≥ last → last duty
 *   - otherwise linear between the bracketing pair, NO rounding
 *   - degenerate vertical pair (Δtemp < f32::EPSILON) → left point's duty
 * The daemon computes in f32 and this model in f64; the parity vectors pinned
 * in curveModel.test.ts are exact in both representations, and editor-range
 * curves stay far inside the Rust tests' 1e-3 tolerance.
 *
 * Editor bounds: the Rust side imposes NO temp bounds (any f32 parses and
 * evaluates), so the editor picks a sane working range of 0–110 °C. Duty is
 * clamped to 0–100 and rounded to an integer by the edit ops; temp is
 * quantized to 0.1 °C so drags don't write noisy floats into config.json.
 * `interpolate` itself never clamps or quantizes its inputs — only the edit
 * ops shape what gets stored.
 */

/** `[temp °C, duty %]` — mirrors config.rs `Curve.points` entries on the wire. */
export type CurvePoint = [temp: number, duty: number];

/** Editor-only temp bounds (the daemon has none — see module doc). */
export const TEMP_MIN = 0;
export const TEMP_MAX = 110;
export const DUTY_MIN = 0;
export const DUTY_MAX = 100;
/** A curve below this many points is not editable down further. */
export const MIN_POINTS = 2;

/** f32::EPSILON — mirrors curve.rs's degenerate-vertical-segment guard. */
export const F32_EPSILON = 1.1920928955078125e-7;

/** Clamp to the editor's temp range and quantize to 0.1 °C. */
export function clampTemp(temp: number): number {
  if (!Number.isFinite(temp)) return TEMP_MIN;
  const clamped = Math.min(TEMP_MAX, Math.max(TEMP_MIN, temp));
  return Math.round(clamped * 10) / 10;
}

/** Clamp to 0–100 and round to an integer duty %. */
export function clampDuty(duty: number): number {
  if (!Number.isFinite(duty)) return DUTY_MIN;
  return Math.round(Math.min(DUTY_MAX, Math.max(DUTY_MIN, duty)));
}

/** Fresh copy, ascending by temp (stable, like Rust's sort_by + total_cmp). */
function sortedCopy(points: readonly CurvePoint[]): CurvePoint[] {
  return points.map((p): CurvePoint => [p[0], p[1]]).sort((a, b) => a[0] - b[0]);
}

/**
 * Move point `idx` to (temp, duty), clamped/quantized. Returns a NEW
 * sorted-by-temp array; the input is never mutated. Out-of-range `idx`
 * returns the input array unchanged (same reference).
 */
export function movePoint(
  points: CurvePoint[],
  idx: number,
  temp: number,
  duty: number,
): CurvePoint[] {
  if (idx < 0 || idx >= points.length) return points;
  const next = points.map((p): CurvePoint => [p[0], p[1]]);
  next[idx] = [clampTemp(temp), clampDuty(duty)];
  return next.sort((a, b) => a[0] - b[0]);
}

/** Add a point at (temp, duty), clamped/quantized. New sorted array. */
export function addPoint(points: CurvePoint[], temp: number, duty: number): CurvePoint[] {
  return sortedCopy([...points, [clampTemp(temp), clampDuty(duty)]]);
}

/**
 * Remove point `idx`. Refuses to shrink a curve below MIN_POINTS (or to act
 * on an out-of-range index): those cases return the input array unchanged
 * (same reference), so callers can detect the no-op with `===`.
 */
export function removePoint(points: CurvePoint[], idx: number): CurvePoint[] {
  if (points.length <= MIN_POINTS) return points;
  if (idx < 0 || idx >= points.length) return points;
  return sortedCopy(points.filter((_, i) => i !== idx));
}

/**
 * Duty % for a temperature — exact mirror of curve.rs `SortedCurve::eval`.
 * Sorts a copy first (SortedCurve sorts at construction), so unsorted config
 * arrays evaluate identically to the daemon. Returns the raw linear result;
 * no rounding (the daemon rounds nothing here either).
 */
export function interpolate(points: CurvePoint[], temp: number): number {
  const pts = sortedCopy(points);
  if (pts.length === 0) return 50;
  if (pts.length === 1) return pts[0][1];
  const first = pts[0];
  const last = pts[pts.length - 1];
  if (temp <= first[0]) return first[1];
  if (temp >= last[0]) return last[1];
  for (let i = 0; i + 1 < pts.length; i++) {
    const [t1, s1] = pts[i];
    const [t2, s2] = pts[i + 1];
    if (temp >= t1 && temp <= t2) {
      if (Math.abs(t2 - t1) < F32_EPSILON) return s1;
      const ratio = (temp - t1) / (t2 - t1);
      return s1 + ratio * (s2 - s1);
    }
  }
  return last[1];
}

/**
 * Duty % → PWM byte — mirrors curve.rs `percent_to_pwm`:
 * `(pct * 2.55) as u8`, i.e. a saturating truncating cast (NaN → 0).
 * Both f32 and f64 represent 2.55 slightly BELOW its decimal value, so the
 * truncation behaves identically for editor-range inputs; the live anchor
 * (34% → 86) is pinned in curveModel.test.ts.
 */
export function percentToPwm(pct: number): number {
  const raw = pct * 2.55;
  if (Number.isNaN(raw)) return 0;
  return Math.min(255, Math.max(0, Math.trunc(raw)));
}
