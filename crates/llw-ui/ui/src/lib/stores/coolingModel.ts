/**
 * Cooling config model — framework-free, pure functions over the config JSON
 * that GetConfig/SetConfig round-trip.
 *
 * Wire shapes (crates/llw-daemon/src/config.rs, verified 2026-07-15):
 *   - `Curve { name, sensor: {hwmon_name, input}, points: [[temp_c, duty_pct], ...] }`
 *   - `DeviceConfig.slots` is a fixed-length-4 array of `SlotSpeed`, which is
 *     serde-UNTAGGED: a JSON number is a constant speed % (`Percent(u8)`), a
 *     JSON string names a curve (`Curve(String)`). 0 = off.
 *
 * Every function here is non-mutating: it returns a NEW config object with
 * only the edited path replaced. All other fields — including fields this
 * module has never heard of (index signatures) — ride through by spread, so
 * a SetConfig of the result byte-preserves everything untouched.
 *
 * No-op convention (mirrors curveModel.ts): a refused or unmatched edit
 * returns the INPUT object unchanged (same reference) so callers can detect
 * it with `===`.
 */

import type { CurvePoint } from './curveModel.js';

/** Untagged SlotSpeed: number = fixed percent, string = curve name. */
export type SlotSpeed = number | string;

/**
 * Wire sentinel for a motherboard-controlled slot (config.rs `SlotSpeed`
 * gained `MbSync`, serialized as the string `"mb"` in M4f). The daemon
 * refuses a curve named `"mb"` — see `isReservedCurveName`.
 */
export const MB_SLOT = 'mb';

/** Default duty (%) when a device returns to software speed control. */
export const SOFTWARE_DEFAULT_PCT = 40;

/**
 * True for names the daemon reserves: a curve may not be called `"mb"`
 * (trimmed, exact — the wire match is case-sensitive). `addCurve` and
 * `renameCurve` refuse these with the usual same-reference no-op; screens
 * check this first so they can toast a precise message.
 */
export function isReservedCurveName(name: string): boolean {
  return name.trim() === MB_SLOT;
}

/** Native hwmon addressing — config.rs `SensorSpec`, verbatim wire shape. */
export interface SensorSpec {
  hwmon_name: string;
  input: string;
}

export interface ConfigCurve {
  name: string;
  sensor: SensorSpec;
  points: CurvePoint[];
  [key: string]: unknown; // future fields pass through untouched
}

export interface ConfigDevice {
  mac: string;
  name?: string | null;
  /** Fixed length 4 on the wire; only the first fan_count entries are real. */
  slots: SlotSpeed[];
  [key: string]: unknown; // color/effect/etc. pass through untouched
}

export interface CoolingConfig {
  schema_version: number;
  curves: ConfigCurve[];
  devices: ConfigDevice[];
  [key: string]: unknown; // control/reliability/observation/presets untouched
}

/** A slot that references a curve by name. */
export interface CurveReference {
  /** Display identity: the device's config name when set, else its MAC. */
  device: string;
  /** Zero-based slot index. */
  slot: number;
}

export type DeleteCurveResult =
  | { ok: true; config: CoolingConfig }
  | { ok: false; blockedBy: CurveReference[] };

/**
 * Starting points for a new curve. The daemon ships no default curve
 * (config.rs `Config::new()` starts with an empty `curves` vec), so this is
 * the editor's own quiet ramp: 20% at 30 °C up to 100% at 70 °C.
 */
export const DEFAULT_CURVE_POINTS: CurvePoint[] = [
  [30, 20],
  [70, 100],
];

function deviceLabel(dev: ConfigDevice): string {
  const name = typeof dev.name === 'string' ? dev.name.trim() : '';
  return name !== '' ? name : dev.mac;
}

/** Every device slot that references curve `name`, in config order. */
export function curveReferences(config: CoolingConfig, name: string): CurveReference[] {
  const refs: CurveReference[] = [];
  for (const dev of config.devices) {
    dev.slots.forEach((slot, i) => {
      if (slot === name) refs.push({ device: deviceLabel(dev), slot: i });
    });
  }
  return refs;
}

/** Replace the named curve's points. Unknown name → no-op (same reference). */
export function setCurvePoints(
  config: CoolingConfig,
  name: string,
  points: CurvePoint[],
): CoolingConfig {
  if (!config.curves.some((c) => c.name === name)) return config;
  return {
    ...config,
    curves: config.curves.map((c) => (c.name === name ? { ...c, points } : c)),
  };
}

/** Rebind the named curve to a sensor. Unknown name → no-op (same reference). */
export function setCurveSensor(
  config: CoolingConfig,
  name: string,
  sensor: SensorSpec,
): CoolingConfig {
  if (!config.curves.some((c) => c.name === name)) return config;
  return {
    ...config,
    curves: config.curves.map((c) => (c.name === name ? { ...c, sensor } : c)),
  };
}

/**
 * Rename a curve AND every device-slot reference to it (the daemon validates
 * referential integrity on SetConfig — a rename must never orphan a slot).
 * No-ops (same reference): unknown `oldName`, blank `newName`, `newName`
 * already taken by another curve, oldName === newName, or the reserved
 * `"mb"`.
 */
export function renameCurve(
  config: CoolingConfig,
  oldName: string,
  newName: string,
): CoolingConfig {
  const trimmed = newName.trim();
  if (trimmed === '' || trimmed === oldName || isReservedCurveName(trimmed)) return config;
  if (!config.curves.some((c) => c.name === oldName)) return config;
  if (config.curves.some((c) => c.name === trimmed)) return config;
  return {
    ...config,
    curves: config.curves.map((c) => (c.name === oldName ? { ...c, name: trimmed } : c)),
    devices: config.devices.map((dev) =>
      dev.slots.some((s) => s === oldName)
        ? { ...dev, slots: dev.slots.map((s) => (s === oldName ? trimmed : s)) }
        : dev,
    ),
  };
}

/**
 * Delete a curve — refused when any device slot still references it
 * (`{ok: false, blockedBy}` lists each referencing device/slot). Deleting a
 * name that does not exist "succeeds" with the config unchanged.
 */
export function deleteCurve(config: CoolingConfig, name: string): DeleteCurveResult {
  const blockedBy = curveReferences(config, name);
  if (blockedBy.length > 0) return { ok: false, blockedBy };
  if (!config.curves.some((c) => c.name === name)) return { ok: true, config };
  return {
    ok: true,
    config: { ...config, curves: config.curves.filter((c) => c.name !== name) },
  };
}

/**
 * Append a new curve. No-ops (same reference): blank name, a name already
 * in use, or the reserved `"mb"`. `points` defaults to
 * [`DEFAULT_CURVE_POINTS`].
 */
export function addCurve(
  config: CoolingConfig,
  name: string,
  sensor: SensorSpec,
  points: CurvePoint[] = DEFAULT_CURVE_POINTS,
): CoolingConfig {
  const trimmed = name.trim();
  if (trimmed === '' || isReservedCurveName(trimmed)) return config;
  if (config.curves.some((c) => c.name === trimmed)) return config;
  return {
    ...config,
    curves: [
      ...config.curves,
      { name: trimmed, sensor, points: points.map((p): CurvePoint => [p[0], p[1]]) },
    ],
  };
}

/**
 * Assign one device slot. A number is clamped to an integer 0–100 (the wire
 * is `Percent(u8)` and the daemon rejects >100); a string names a curve —
 * the caller offers only existing names, and the daemon re-validates.
 * No-ops (same reference): unknown MAC or slot index outside the array.
 */
export function setSlot(
  config: CoolingConfig,
  deviceMac: string,
  slotIdx: number,
  speed: SlotSpeed,
): CoolingConfig {
  const dev = config.devices.find((d) => d.mac === deviceMac);
  if (!dev || slotIdx < 0 || slotIdx >= dev.slots.length) return config;
  const value =
    typeof speed === 'number'
      ? Math.round(Math.min(100, Math.max(0, Number.isFinite(speed) ? speed : 0)))
      : speed;
  return {
    ...config,
    devices: config.devices.map((d) =>
      d.mac === deviceMac
        ? { ...d, slots: d.slots.map((s, i) => (i === slotIdx ? value : s)) }
        : d,
    ),
  };
}

/**
 * Hand the whole device to its motherboard header: EVERY slot (active and
 * spare — the daemon derives `speed_source` from the full set) becomes the
 * `"mb"` sentinel. No-op (same reference) only for an unknown MAC — an
 * already-mb device still yields a fresh object, so callers can round-trip
 * unconditionally.
 */
export function setDeviceAllSlotsMb(config: CoolingConfig, deviceMac: string): CoolingConfig {
  if (!config.devices.some((d) => d.mac === deviceMac)) return config;
  return {
    ...config,
    devices: config.devices.map((d) =>
      d.mac === deviceMac ? { ...d, slots: d.slots.map((): SlotSpeed => MB_SLOT) } : d,
    ),
  };
}

/**
 * Return a device to software speed control: active slots (index <
 * `fanCount`) become `pct` (clamped to an integer 0–100, default 40%), spare
 * slots become 0. No-op (same reference) only for an unknown MAC.
 */
export function setDeviceSoftwareSlots(
  config: CoolingConfig,
  deviceMac: string,
  fanCount: number,
  pct: number = SOFTWARE_DEFAULT_PCT,
): CoolingConfig {
  if (!config.devices.some((d) => d.mac === deviceMac)) return config;
  const value = Math.round(Math.min(100, Math.max(0, Number.isFinite(pct) ? pct : 0)));
  return {
    ...config,
    devices: config.devices.map((d) =>
      d.mac === deviceMac
        ? { ...d, slots: d.slots.map((_, i): SlotSpeed => (i < fanCount ? value : 0)) }
        : d,
    ),
  };
}
