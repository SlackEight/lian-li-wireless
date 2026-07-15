import { describe, it, expect } from 'vitest';
import {
  type CoolingConfig,
  DEFAULT_CURVE_POINTS,
  curveReferences,
  setCurvePoints,
  setCurveSensor,
  renameCurve,
  deleteCurve,
  addCurve,
  setSlot,
} from './coolingModel.js';

/**
 * Fixture mirroring a real daemon config: untagged SlotSpeed (number =
 * percent, string = curve name), fixed-length-4 slots, plus unknown fields at
 * every level that MUST survive edits byte-identical (the SetConfig
 * round-trip carries the whole document).
 */
function sample(): CoolingConfig {
  return {
    schema_version: 1,
    curves: [
      {
        name: 'cpu',
        sensor: { hwmon_name: 'k10temp', input: 'temp1_input' },
        points: [
          [29, 30],
          [89, 37],
        ],
        future_curve_field: 'keep me',
      },
      {
        name: 'gpu',
        sensor: { hwmon_name: 'amdgpu', input: 'temp2_input' },
        points: [
          [40, 20],
          [80, 100],
        ],
      },
    ],
    devices: [
      {
        mac: '02:8b:51:62:32:e1',
        name: 'top fans',
        slots: ['cpu', 'cpu', 40, 0],
        color: { rgb: [255, 255, 255], brightness: 4 },
      },
      {
        mac: '02:8b:51:62:32:e2',
        name: null,
        slots: [0, 'gpu', 'cpu', 0],
      },
    ],
    control: { tick_ms: 1000, hysteresis_temp: 1.0 },
    presets: [{ name: 'ocean', effect: { kind: 'meteor' } }],
  };
}

/** Deep-compare helper: everything except the named top-level keys is identical. */
function expectUntouchedExcept(before: CoolingConfig, after: CoolingConfig, touched: string[]) {
  for (const key of Object.keys(before)) {
    if (!touched.includes(key)) {
      expect(after[key]).toEqual(before[key]);
    }
  }
}

describe('curveReferences', () => {
  it('lists every referencing device/slot, using config name else mac', () => {
    expect(curveReferences(sample(), 'cpu')).toEqual([
      { device: 'top fans', slot: 0 },
      { device: 'top fans', slot: 1 },
      { device: '02:8b:51:62:32:e2', slot: 2 },
    ]);
    expect(curveReferences(sample(), 'gpu')).toEqual([{ device: '02:8b:51:62:32:e2', slot: 1 }]);
  });

  it('percent slots never count as references, even numerically odd ones', () => {
    expect(curveReferences(sample(), 'nope')).toEqual([]);
  });
});

describe('setCurvePoints', () => {
  it('replaces only the named curve, preserving its unknown fields', () => {
    const cfg = sample();
    const next = setCurvePoints(cfg, 'cpu', [
      [30, 20],
      [70, 100],
    ]);
    expect(next).not.toBe(cfg);
    expect(next.curves[0].points).toEqual([
      [30, 20],
      [70, 100],
    ]);
    expect(next.curves[0].future_curve_field).toBe('keep me');
    expect(next.curves[1]).toEqual(cfg.curves[1]);
    expectUntouchedExcept(cfg, next, ['curves']);
    // input untouched
    expect(cfg.curves[0].points).toEqual([
      [29, 30],
      [89, 37],
    ]);
  });

  it('unknown curve name is a no-op returning the same reference', () => {
    const cfg = sample();
    expect(setCurvePoints(cfg, 'nope', [[0, 0]])).toBe(cfg);
  });
});

describe('setCurveSensor', () => {
  it('rebinds only the named curve', () => {
    const cfg = sample();
    const next = setCurveSensor(cfg, 'gpu', { hwmon_name: 'nvme', input: 'temp1_input' });
    expect(next.curves[1].sensor).toEqual({ hwmon_name: 'nvme', input: 'temp1_input' });
    expect(next.curves[0]).toEqual(cfg.curves[0]);
    expectUntouchedExcept(cfg, next, ['curves']);
  });

  it('unknown curve name is a no-op returning the same reference', () => {
    const cfg = sample();
    expect(setCurveSensor(cfg, 'nope', { hwmon_name: 'x', input: 'y' })).toBe(cfg);
  });
});

describe('renameCurve', () => {
  it('renames the curve AND updates every slot reference', () => {
    const cfg = sample();
    const next = renameCurve(cfg, 'cpu', 'processor');
    expect(next.curves[0].name).toBe('processor');
    expect(next.devices[0].slots).toEqual(['processor', 'processor', 40, 0]);
    expect(next.devices[1].slots).toEqual([0, 'gpu', 'processor', 0]);
    // referential integrity: nothing still points at the old name
    expect(curveReferences(next, 'cpu')).toEqual([]);
    expectUntouchedExcept(cfg, next, ['curves', 'devices']);
    // unknown fields on the renamed curve and devices survive
    expect(next.curves[0].future_curve_field).toBe('keep me');
    expect(next.devices[0].color).toEqual(cfg.devices[0].color);
    // input untouched
    expect(cfg.curves[0].name).toBe('cpu');
    expect(cfg.devices[0].slots).toEqual(['cpu', 'cpu', 40, 0]);
  });

  it('trims the new name', () => {
    const next = renameCurve(sample(), 'gpu', '  graphics  ');
    expect(next.curves[1].name).toBe('graphics');
    expect(next.devices[1].slots[1]).toBe('graphics');
  });

  it('no-ops (same reference): blank, collision, unknown old name, identity', () => {
    const cfg = sample();
    expect(renameCurve(cfg, 'cpu', '')).toBe(cfg);
    expect(renameCurve(cfg, 'cpu', '   ')).toBe(cfg);
    expect(renameCurve(cfg, 'cpu', 'gpu')).toBe(cfg); // would merge two curves' refs
    expect(renameCurve(cfg, 'nope', 'anything')).toBe(cfg);
    expect(renameCurve(cfg, 'cpu', 'cpu')).toBe(cfg);
  });
});

describe('deleteCurve', () => {
  it('refuses while referenced, naming each blocking device/slot', () => {
    const res = deleteCurve(sample(), 'cpu');
    expect(res.ok).toBe(false);
    if (!res.ok) {
      expect(res.blockedBy).toEqual([
        { device: 'top fans', slot: 0 },
        { device: 'top fans', slot: 1 },
        { device: '02:8b:51:62:32:e2', slot: 2 },
      ]);
    }
  });

  it('deletes once every reference is gone', () => {
    const cfg = sample();
    // point the lone gpu reference at a fixed percent first
    const unreferenced = setSlot(cfg, '02:8b:51:62:32:e2', 1, 35);
    const res = deleteCurve(unreferenced, 'gpu');
    expect(res.ok).toBe(true);
    if (res.ok) {
      expect(res.config.curves.map((c) => c.name)).toEqual(['cpu']);
      expectUntouchedExcept(unreferenced, res.config, ['curves']);
    }
    // input untouched
    expect(unreferenced.curves).toHaveLength(2);
  });

  it('deleting a nonexistent curve succeeds with the config unchanged', () => {
    const cfg = sample();
    const res = deleteCurve(cfg, 'nope');
    expect(res.ok).toBe(true);
    if (res.ok) expect(res.config).toBe(cfg);
  });
});

describe('addCurve', () => {
  const sensor = { hwmon_name: 'k10temp', input: 'temp1_input' };

  it('appends with the default points when none given', () => {
    const cfg = sample();
    const next = addCurve(cfg, 'case', sensor);
    expect(next.curves).toHaveLength(3);
    expect(next.curves[2]).toEqual({ name: 'case', sensor, points: DEFAULT_CURVE_POINTS });
    // the stored points are a copy — editing them later must not mutate the constant
    expect(next.curves[2].points).not.toBe(DEFAULT_CURVE_POINTS);
    expectUntouchedExcept(cfg, next, ['curves']);
    expect(cfg.curves).toHaveLength(2);
  });

  it('accepts explicit points and trims the name', () => {
    const next = addCurve(sample(), '  pump ', sensor, [
      [20, 50],
      [60, 100],
    ]);
    expect(next.curves[2].name).toBe('pump');
    expect(next.curves[2].points).toEqual([
      [20, 50],
      [60, 100],
    ]);
  });

  it('no-ops (same reference): blank name, duplicate name', () => {
    const cfg = sample();
    expect(addCurve(cfg, '', sensor)).toBe(cfg);
    expect(addCurve(cfg, '   ', sensor)).toBe(cfg);
    expect(addCurve(cfg, 'cpu', sensor)).toBe(cfg);
    expect(addCurve(cfg, ' cpu ', sensor)).toBe(cfg); // trims before checking
  });
});

describe('setSlot', () => {
  it('sets a fixed percent, clamped to an integer 0–100', () => {
    const cfg = sample();
    const next = setSlot(cfg, '02:8b:51:62:32:e1', 2, 55);
    expect(next.devices[0].slots).toEqual(['cpu', 'cpu', 55, 0]);
    expect(setSlot(cfg, '02:8b:51:62:32:e1', 2, 150).devices[0].slots[2]).toBe(100);
    expect(setSlot(cfg, '02:8b:51:62:32:e1', 2, -5).devices[0].slots[2]).toBe(0);
    expect(setSlot(cfg, '02:8b:51:62:32:e1', 2, 42.6).devices[0].slots[2]).toBe(43);
    expect(setSlot(cfg, '02:8b:51:62:32:e1', 2, Number.NaN).devices[0].slots[2]).toBe(0);
    // input untouched
    expect(cfg.devices[0].slots[2]).toBe(40);
  });

  it('assigns a curve by name, leaving other slots and devices alone', () => {
    const cfg = sample();
    const next = setSlot(cfg, '02:8b:51:62:32:e2', 0, 'gpu');
    expect(next.devices[1].slots).toEqual(['gpu', 'gpu', 'cpu', 0]);
    expect(next.devices[0]).toEqual(cfg.devices[0]);
    expectUntouchedExcept(cfg, next, ['devices']);
    // unknown device fields survive
    expect(next.devices[1].name).toBeNull();
  });

  it('no-ops (same reference): unknown mac, slot index out of range', () => {
    const cfg = sample();
    expect(setSlot(cfg, 'ff:ff:ff:ff:ff:ff', 0, 50)).toBe(cfg);
    expect(setSlot(cfg, '02:8b:51:62:32:e1', 4, 50)).toBe(cfg);
    expect(setSlot(cfg, '02:8b:51:62:32:e1', -1, 50)).toBe(cfg);
  });
});

describe('save payload shape', () => {
  it('a chain of edits still round-trips every unknown field byte-identically', () => {
    const cfg = sample();
    let next = addCurve(cfg, 'case', { hwmon_name: 'nct6799', input: 'temp3_input' });
    next = setSlot(next, '02:8b:51:62:32:e1', 3, 'case');
    next = renameCurve(next, 'case', 'chassis');
    next = setCurvePoints(next, 'chassis', [
      [25, 0],
      [75, 80],
    ]);
    next = setCurveSensor(next, 'chassis', { hwmon_name: 'nct6799', input: 'temp1_input' });

    // untouched top-level sections are byte-identical through the whole chain
    expect(JSON.stringify(next.control)).toBe(JSON.stringify(cfg.control));
    expect(JSON.stringify(next.presets)).toBe(JSON.stringify(cfg.presets));
    expect(next.schema_version).toBe(1);
    // untouched nested unknowns too
    expect(next.curves[0].future_curve_field).toBe('keep me');
    expect(next.devices[0].color).toEqual(cfg.devices[0].color);
    // and the edits landed
    expect(next.devices[0].slots[3]).toBe('chassis');
    expect(next.curves[2]).toEqual({
      name: 'chassis',
      sensor: { hwmon_name: 'nct6799', input: 'temp1_input' },
      points: [
        [25, 0],
        [75, 80],
      ],
    });
    // wire-shape sanity: slots stay untagged scalars
    for (const dev of next.devices) {
      for (const slot of dev.slots) {
        expect(['number', 'string']).toContain(typeof slot);
      }
    }
  });
});
