import { describe, it, expect } from 'vitest';
import {
  type CurvePoint,
  TEMP_MAX,
  MIN_POINTS,
  movePoint,
  addPoint,
  removePoint,
  interpolate,
  percentToPwm,
} from './curveModel.js';

/**
 * Parity vectors transcribed VERBATIM from crates/llw-daemon/src/curve.rs
 * tests (its `owner_curve()` fixture and assertions). If these drift, the
 * editor preview and the daemon's actual fan behavior have diverged — fix
 * the model, never these numbers.
 */

// curve.rs `owner_curve()`: the owner's real curve-1, stored UNSORTED exactly
// as in their config (also proves interpolate sorts like SortedCurve::new).
const OWNER_CURVE: CurvePoint[] = [
  [29.0, 30.0],
  [52.0, 34.0],
  [69.0, 35.0],
  [89.0, 37.0],
  [40.0, 34.0],
  [78.0, 35.0],
];

describe('interpolation parity with curve.rs', () => {
  // Rust test: `owner_curve_live_anchor`
  it('live anchor: 41.3 °C → 34% → PWM 86 (observed on real hardware)', () => {
    const pct = interpolate(OWNER_CURVE, 41.3); // between (40,34) and (52,34) → 34
    expect(Math.abs(pct - 34.0)).toBeLessThan(0.001);
    // curve.rs works in duty % end-to-end; PWM bytes only exist at the wire
    // via percent_to_pwm — mirrored here, no unit conversion needed.
    expect(percentToPwm(pct)).toBe(86);
  });

  // Rust test: `interpolation_boundaries`
  it('boundaries: below min → min duty, above max → max duty, midpoint linear', () => {
    expect(Math.abs(interpolate(OWNER_CURVE, 10.0) - 30.0)).toBeLessThan(0.001);
    expect(Math.abs(interpolate(OWNER_CURVE, 95.0) - 37.0)).toBeLessThan(0.001);
    // midpoint of (29,30)-(40,34): temp 34.5 → 30 + 0.5*4 = 32
    expect(Math.abs(interpolate(OWNER_CURVE, 34.5) - 32.0)).toBeLessThan(0.001);
  });

  // Rust test: `degenerate_curves`
  it('degenerate: empty curve → 50, single point → its duty', () => {
    expect(Math.abs(interpolate([], 50.0) - 50.0)).toBeLessThan(0.001);
    expect(Math.abs(interpolate([[40.0, 25.0]], 99.0) - 25.0)).toBeLessThan(0.001);
  });

  // Not a transcribed Rust test — exercises the same defensive branch as
  // curve.rs's `(t2 - t1).abs() < f32::EPSILON` guard (→ left duty).
  it('near-vertical segment returns the left duty (f32::EPSILON guard)', () => {
    const pts: CurvePoint[] = [
      [30, 10],
      [30.00000005, 90],
    ];
    expect(interpolate(pts, 30.00000002)).toBe(10);
  });

  it('does not round: linear results stay fractional like eval()', () => {
    // between (29,30) and (40,34): 30 + (33-29)/11 * 4 = 31.4545…
    expect(interpolate(OWNER_CURVE, 33)).toBeCloseTo(30 + (4 * 4) / 11, 10);
  });
});

describe('movePoint', () => {
  const pts: CurvePoint[] = [
    [20, 10],
    [40, 50],
    [60, 90],
  ];

  it('returns a new array and never mutates the input', () => {
    const next = movePoint(pts, 1, 45, 55);
    expect(next).not.toBe(pts);
    expect(next).toEqual([
      [20, 10],
      [45, 55],
      [60, 90],
    ]);
    expect(pts).toEqual([
      [20, 10],
      [40, 50],
      [60, 90],
    ]);
  });

  it('re-sorts when a point is dragged past a neighbour', () => {
    expect(movePoint(pts, 0, 50, 10)).toEqual([
      [40, 50],
      [50, 10],
      [60, 90],
    ]);
  });

  it('clamps duty to 0–100 integers', () => {
    expect(movePoint(pts, 0, 20, 150)[0]).toEqual([20, 100]);
    expect(movePoint(pts, 0, 20, -5)[0]).toEqual([20, 0]);
    expect(movePoint(pts, 0, 20, 42.6)[0]).toEqual([20, 43]);
  });

  it('clamps temp to editor bounds (0–110) and quantizes to 0.1 °C', () => {
    expect(movePoint(pts, 0, -3, 10)[0]).toEqual([0, 10]);
    expect(movePoint(pts, 2, 200, 90)[2]).toEqual([TEMP_MAX, 90]);
    expect(movePoint(pts, 1, 33.333, 50)[1]).toEqual([33.3, 50]);
  });

  it('sorts even when the input was unsorted (e.g. straight from config)', () => {
    const next = movePoint([...OWNER_CURVE], 0, 29, 31);
    const temps = next.map((p) => p[0]);
    expect(temps).toEqual([...temps].sort((a, b) => a - b));
  });

  it('out-of-range index is a no-op returning the same reference', () => {
    expect(movePoint(pts, 3, 10, 10)).toBe(pts);
    expect(movePoint(pts, -1, 10, 10)).toBe(pts);
  });
});

describe('addPoint', () => {
  it('inserts in sorted position with clamps applied', () => {
    const pts: CurvePoint[] = [
      [20, 10],
      [60, 90],
    ];
    expect(addPoint(pts, 40.04, 55.5)).toEqual([
      [20, 10],
      [40, 56],
      [60, 90],
    ]);
    expect(addPoint(pts, 200, -5)).toEqual([
      [20, 10],
      [60, 90],
      [110, 0],
    ]);
    expect(pts).toHaveLength(2); // input untouched
  });
});

describe('removePoint', () => {
  const pts: CurvePoint[] = [
    [20, 10],
    [40, 50],
    [60, 90],
  ];

  it('removes by index, returning a new sorted array', () => {
    expect(removePoint(pts, 1)).toEqual([
      [20, 10],
      [60, 90],
    ]);
    expect(pts).toHaveLength(3);
  });

  it(`refuses below ${MIN_POINTS} points — returns the same reference`, () => {
    const two: CurvePoint[] = [
      [20, 10],
      [40, 50],
    ];
    expect(removePoint(two, 0)).toBe(two);
    expect(removePoint(two, 1)).toBe(two);
  });

  it('out-of-range index is a no-op returning the same reference', () => {
    expect(removePoint(pts, 3)).toBe(pts);
    expect(removePoint(pts, -1)).toBe(pts);
  });
});
