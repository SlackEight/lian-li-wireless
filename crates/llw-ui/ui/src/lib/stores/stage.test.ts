import { describe, it, expect } from 'vitest';
import {
  EFFECT_KINDS,
  MAX_PALETTE,
  createStageStore,
  defaultSpec,
  frameBudget,
  geometryForKind,
  geometryLeds,
  type Geometry,
  type Rgb,
} from './stage.js';

describe('default spec', () => {
  it('is ripple, palette ≤8, speed 3, forward, brightness 4 (serde defaults)', () => {
    const spec = createStageStore().getSnapshot().spec;
    expect(spec.kind).toBe('ripple');
    expect(spec.colors.length).toBeGreaterThan(0);
    expect(spec.colors.length).toBeLessThanOrEqual(MAX_PALETTE);
    expect(spec.speed).toBe(3);
    expect(spec.direction).toBe('forward');
    expect(spec.brightness).toBe(4);
  });

  it('defaultSpec() returns a fresh object each call (no shared mutable state)', () => {
    const a = defaultSpec();
    const b = defaultSpec();
    expect(a).not.toBe(b);
    expect(a.colors).not.toBe(b.colors);
    expect(a).toEqual(b);
  });

  it('kind catalog matches llw-effects wire names in definition order', () => {
    expect(EFFECT_KINDS).toEqual([
      'static',
      'breathing',
      'color-cycle',
      'rainbow-morph',
      'rainbow',
      'meteor',
      'runway',
      'ripple',
    ]);
  });
});

describe('setSpec clamps', () => {
  it('speed clamps to 1..=5 integers', () => {
    const store = createStageStore();
    store.setSpec({ speed: 0 });
    expect(store.getSnapshot().spec.speed).toBe(1);
    store.setSpec({ speed: 9 });
    expect(store.getSnapshot().spec.speed).toBe(5);
    store.setSpec({ speed: 2.6 });
    expect(store.getSnapshot().spec.speed).toBe(3);
  });

  it('non-finite speed falls back to the serde default (3)', () => {
    const store = createStageStore();
    store.setSpec({ speed: 5 });
    store.setSpec({ speed: NaN });
    expect(store.getSnapshot().spec.speed).toBe(3);
  });

  it('brightness clamps to 0..=4 integers', () => {
    const store = createStageStore();
    store.setSpec({ brightness: -1 });
    expect(store.getSnapshot().spec.brightness).toBe(0);
    store.setSpec({ brightness: 9 });
    expect(store.getSnapshot().spec.brightness).toBe(4);
    store.setSpec({ brightness: 3.4 });
    expect(store.getSnapshot().spec.brightness).toBe(3);
  });

  it('palette is truncated to 8 entries', () => {
    const store = createStageStore();
    const nine: Rgb[] = Array.from({ length: 9 }, (_, i) => [i, i, i]);
    store.setSpec({ colors: nine });
    const colors = store.getSnapshot().spec.colors;
    expect(colors.length).toBe(MAX_PALETTE);
    expect(colors[7]).toEqual([7, 7, 7]);
  });

  it('color channels clamp to 0..=255 integers', () => {
    const store = createStageStore();
    store.setSpec({ colors: [[-5, 300, 12.7]] });
    expect(store.getSnapshot().spec.colors).toEqual([[0, 255, 13]]);
  });

  it('kind and direction pass through', () => {
    const store = createStageStore();
    store.setSpec({ kind: 'meteor', direction: 'reverse' });
    const spec = store.getSnapshot().spec;
    expect(spec.kind).toBe('meteor');
    expect(spec.direction).toBe('reverse');
  });

  it('notifies subscribers on a real change, with a new snapshot object', () => {
    const store = createStageStore();
    const before = store.getSnapshot();
    let calls = 0;
    store.subscribe(() => {
      calls += 1;
    });
    store.setSpec({ speed: 5 });
    expect(calls).toBe(1);
    expect(store.getSnapshot()).not.toBe(before);
    expect(before.spec.speed).toBe(3); // old snapshot untouched
  });

  it('does not notify when the clamped patch is a no-op', () => {
    const store = createStageStore();
    let calls = 0;
    store.subscribe(() => {
      calls += 1;
    });
    store.setSpec({ speed: 3 }); // already 3
    store.setSpec({ speed: 3.2 }); // rounds to 3
    expect(calls).toBe(0);
  });

  it('unsubscribe stops notifications', () => {
    const store = createStageStore();
    let calls = 0;
    const unsub = store.subscribe(() => {
      calls += 1;
    });
    unsub();
    store.setSpec({ speed: 5 });
    expect(calls).toBe(0);
  });
});

describe('selectDevice', () => {
  it('sets the mac and notifies; spec is untouched', () => {
    const store = createStageStore();
    const spec = store.getSnapshot().spec;
    let calls = 0;
    store.subscribe(() => {
      calls += 1;
    });
    store.selectDevice('02:8b:51:62:32:e1');
    expect(calls).toBe(1);
    expect(store.getSnapshot().selectedMac).toBe('02:8b:51:62:32:e1');
    expect(store.getSnapshot().spec).toBe(spec);
  });

  it('re-selecting the same mac does not notify', () => {
    const store = createStageStore();
    store.selectDevice('02:8b:51:62:32:e1');
    let calls = 0;
    store.subscribe(() => {
      calls += 1;
    });
    store.selectDevice('02:8b:51:62:32:e1');
    expect(calls).toBe(0);
  });

  it('null clears the selection', () => {
    const store = createStageStore();
    store.selectDevice('02:8b:51:62:32:e1');
    store.selectDevice(null);
    expect(store.getSnapshot().selectedMac).toBeNull();
  });
});

/*
 * Geometry mapping — mirrors effects_bridge::geometry_of over the
 * DeviceKind::display_name() strings the status endpoint emits. Cases
 * transcribed from effects_bridge.rs / device_kind.rs tests.
 */
describe('geometryForKind (effects_bridge::geometry_of parity)', () => {
  it('SL-INF 3 fans → 3×44 sl_inf44 (Rust sl_inf_3fan_geometry)', () => {
    const geom = geometryForKind('UNI FAN SL-INF Wireless', 3);
    expect(geom).toEqual({ type: 'fans', fan_count: 3, leds_per_fan: 44, layout: 'sl_inf44' });
    expect(geometryLeds(geom as Geometry)).toBe(132);
  });

  it('SLV3 LED and LCD → 40 LEDs/fan uniform_ring', () => {
    expect(geometryForKind('UNI FAN SL V3 Wireless', 2)).toEqual({
      type: 'fans',
      fan_count: 2,
      leds_per_fan: 40,
      layout: 'uniform_ring',
    });
    expect(geometryForKind('UNI FAN SL V3 Wireless LCD', 1)).toEqual({
      type: 'fans',
      fan_count: 1,
      leds_per_fan: 40,
      layout: 'uniform_ring',
    });
  });

  it('TLV2 LED and LCD → 26 LEDs/fan uniform_ring', () => {
    expect(geometryForKind('UNI FAN TL Wireless', 3)).toEqual({
      type: 'fans',
      fan_count: 3,
      leds_per_fan: 26,
      layout: 'uniform_ring',
    });
    expect(geometryForKind('UNI FAN TL Wireless LCD', 2)).toEqual({
      type: 'fans',
      fan_count: 2,
      leds_per_fan: 26,
      layout: 'uniform_ring',
    });
  });

  it('CLV1 → 24 LEDs/fan uniform_ring', () => {
    expect(geometryForKind('UNI FAN CL Wireless', 4)).toEqual({
      type: 'fans',
      fan_count: 4,
      leds_per_fan: 24,
      layout: 'uniform_ring',
    });
  });

  it('Unknown ("Wireless Device") → 20-LED guess, like the daemon', () => {
    expect(geometryForKind('Wireless Device', 2)).toEqual({
      type: 'fans',
      fan_count: 2,
      leds_per_fan: 20,
      layout: 'uniform_ring',
    });
  });

  it('AIOs → null (Rust aio_geometry_is_none, post-v1)', () => {
    expect(geometryForKind('HydroShift II LCD-C (Wireless)', 1)).toBeNull();
    expect(geometryForKind('HydroShift II LCD-S (Wireless)', 2)).toBeNull();
  });

  it('flat-buffer case devices → strip with led_count_override totals', () => {
    expect(geometryForKind('Lancool 217 Wireless', 0)).toEqual({ type: 'strip', total: 96 });
    expect(geometryForKind('Universal Screen 8.8" Wireless', 0)).toEqual({
      type: 'strip',
      total: 88,
    });
    expect(geometryForKind('Lancool V150 Wireless', 2)).toEqual({ type: 'strip', total: 88 });
  });

  it('Strimer → null (subtype-dependent LED count not in the kind string)', () => {
    // DIVERGES from the daemon on purpose: geometry_of sees DeviceKind::
    // Strimer(n) and picks 116/132/174/88, but display_name() drops n.
    expect(geometryForKind('Strimer Wireless', 0)).toBeNull();
  });

  it('fan device reporting zero fans → null (mirrors the fan_count guard)', () => {
    expect(geometryForKind('UNI FAN SL-INF Wireless', 0)).toBeNull();
  });

  it('unrecognised kind string → null', () => {
    expect(geometryForKind('Flux Capacitor Wireless', 3)).toBeNull();
    // Object-prototype keys must not leak a mapping.
    expect(geometryForKind('toString', 3)).toBeNull();
  });
});

/*
 * Frame budget — clamp(28_000 / (leds × 3), 8, 96), anchor values transcribed
 * from effects_bridge.rs frame_budget tests.
 */
describe('frameBudget (effects_bridge::frame_budget parity)', () => {
  const slInf = (fans: number): Geometry => ({
    type: 'fans',
    fan_count: fans,
    leds_per_fan: 44,
    layout: 'sl_inf44',
  });

  it('132 LEDs (SL-INF 3×44) → 70', () => {
    expect(frameBudget(slInf(3))).toBe(70);
  });

  it('174 LEDs (Strimer-3-sized strip) → 53', () => {
    expect(frameBudget({ type: 'strip', total: 174 })).toBe(53);
  });

  it('44 LEDs (1 fan) → capped at 96', () => {
    expect(frameBudget(slInf(1))).toBe(96);
  });

  it('4000 LEDs → floored at 8', () => {
    expect(frameBudget({ type: 'strip', total: 4000 })).toBe(8);
  });

  it('zero LEDs → 8 (daemon guard)', () => {
    expect(frameBudget({ type: 'strip', total: 0 })).toBe(8);
  });

  it('strip 132 matches the fan case with the same LED count', () => {
    expect(frameBudget({ type: 'strip', total: 132 })).toBe(70);
  });
});
