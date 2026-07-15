/**
 * Stage store — the Lighting screen's working state: which device is on
 * stage and the effect spec being edited (local only; Apply lands in the
 * effect rail, Task C4). Framework-free on purpose: Vitest exercises the
 * clamps and geometry mapping directly; React consumes the singleton through
 * `useSyncExternalStore` (the `useStage()` hook lives in Lighting.tsx).
 *
 * Also home to the pure geometry helpers the stage needs:
 *  - `geometryForKind`: status `kind` string → llw-effects `Geometry`,
 *    mirroring llw-daemon's `effects_bridge::geometry_of`
 *  - `frameBudget`: hardware frame budget, mirroring
 *    `effects_bridge::frame_budget` — the preview renders the SAME frame
 *    count the daemon would upload, so what you see is what plays.
 */

/* ── EffectSpec — mirror of llw-effects' serde shapes (lib.rs, verified
      2026-07-15). All fields except `kind` carry serde defaults, so partial
      JSON like `{"kind":"ripple"}` is valid on the wire; the store keeps a
      fully-populated spec so the UI never depends on implicit defaults. ── */

export type Rgb = [number, number, number];

/** The eight v1 effect kinds, kebab-case wire names in definition order
 * (llw-effects `EffectKind`, `#[serde(rename_all = "kebab-case")]`;
 * `Static` renames to `"static"`). */
export const EFFECT_KINDS = [
  'static',
  'breathing',
  'color-cycle',
  'rainbow-morph',
  'rainbow',
  'meteor',
  'runway',
  'ripple',
] as const;
export type EffectKind = (typeof EFFECT_KINDS)[number];

/** llw-effects `Direction`, `#[serde(rename_all = "lowercase")]`;
 * serde default `forward`. Only Rainbow/Meteor/Runway are directional. */
export type Direction = 'forward' | 'reverse';

export interface EffectSpec {
  /** Required on the wire — the only field without a serde default. */
  kind: EffectKind;
  /** serde default `[]`; the daemon validator enforces ≤ 8 entries. */
  colors: Rgb[];
  /** 1..=5 (clamped by the engine); serde default 3 (3 000 ms period). */
  speed: number;
  /** serde default `forward`. */
  direction: Direction;
  /** 0..=4, a ×(brightness/4) post-render scale; serde default 4. */
  brightness: number;
}

/** Daemon validator limit on palette length. */
export const MAX_PALETTE = 8;

/** Serde defaults (lib.rs `default_speed` / `default_brightness`). */
export const DEFAULT_SPEED = 3;
export const DEFAULT_BRIGHTNESS = 4;

/** Fresh default working spec: ripple over the house blue→violet palette
 * (the palette effects_bridge's own tests demo with), medium speed, full
 * brightness. */
export function defaultSpec(): EffectSpec {
  return {
    kind: 'ripple',
    colors: [
      [0, 0, 255],
      [136, 0, 255],
    ],
    speed: DEFAULT_SPEED,
    direction: 'forward',
    brightness: DEFAULT_BRIGHTNESS,
  };
}

/* ── Clamps ── */

function clampInt(v: number, min: number, max: number, fallback: number): number {
  if (!Number.isFinite(v)) return fallback;
  return Math.min(max, Math.max(min, Math.round(v)));
}

function clampChannel(v: number): number {
  return clampInt(v, 0, 255, 0);
}

/** Normalise a spec to what the daemon would accept: speed 1–5 int,
 * brightness 0–4 int, palette ≤ 8 colors of [r,g,b] 0–255 ints. Non-finite
 * numbers fall back to the serde defaults. Always returns a new object. */
export function clampSpec(spec: EffectSpec): EffectSpec {
  return {
    kind: spec.kind,
    colors: spec.colors
      .slice(0, MAX_PALETTE)
      .map(([r, g, b]): Rgb => [clampChannel(r), clampChannel(g), clampChannel(b)]),
    speed: clampInt(spec.speed, 1, 5, DEFAULT_SPEED),
    direction: spec.direction === 'reverse' ? 'reverse' : 'forward',
    brightness: clampInt(spec.brightness, 0, 4, DEFAULT_BRIGHTNESS),
  };
}

function specsEqual(a: EffectSpec, b: EffectSpec): boolean {
  return (
    a.kind === b.kind &&
    a.speed === b.speed &&
    a.direction === b.direction &&
    a.brightness === b.brightness &&
    a.colors.length === b.colors.length &&
    a.colors.every((c, i) => c[0] === b.colors[i][0] && c[1] === b.colors[i][1] && c[2] === b.colors[i][2])
  );
}

/* ── Geometry — mirror of llw-effects' serde shapes (geometry.rs):
      internally tagged `type`, snake_case variants and layouts. ── */

export type FanLayout = 'uniform_ring' | 'sl_inf44';

export type Geometry =
  | { type: 'fans'; fan_count: number; leds_per_fan: number; layout: FanLayout }
  | { type: 'strip'; total: number };

/** Total LED count (llw-effects `Geometry::len`). */
export function geometryLeds(geom: Geometry): number {
  return geom.type === 'fans' ? geom.fan_count * geom.leds_per_fan : geom.total;
}

/*
 * Status `kind` strings are `DeviceKind::display_name()` (supervisor.rs fills
 * DeviceStatus.kind with it). This table mirrors effects_bridge::geometry_of
 * ∘ device_kind.rs over those display names:
 *
 *   display name                        → geometry
 *   "UNI FAN SL-INF Wireless"           → fans ×44/fan, sl_inf44 (measured wiring)
 *   "UNI FAN SL V3 Wireless"            → fans ×40/fan, uniform_ring
 *   "UNI FAN SL V3 Wireless LCD"        → fans ×40/fan, uniform_ring
 *   "UNI FAN TL Wireless"               → fans ×26/fan, uniform_ring
 *   "UNI FAN TL Wireless LCD"           → fans ×26/fan, uniform_ring
 *   "UNI FAN CL Wireless"               → fans ×24/fan, uniform_ring
 *   "Wireless Device" (Unknown)         → fans ×20/fan, uniform_ring (guess, like the daemon)
 *   "Lancool 217 Wireless"              → strip 96
 *   "Universal Screen 8.8\" Wireless"   → strip 88
 *   "Lancool V150 Wireless"             → strip 88
 *   "HydroShift II LCD-C/S (Wireless)"  → null (AIO composite geometry is post-v1)
 *   "Strimer Wireless"                  → null — DIVERGES from the daemon: the LED
 *       count is subtype-dependent (116/132/174/88) and the status kind string
 *       does not carry the subtype, so the UI cannot know the strip length.
 *   anything else                       → null
 */

const FAN_KINDS = new Map<string, { lpf: number; layout: FanLayout }>([
  ['UNI FAN SL V3 Wireless', { lpf: 40, layout: 'uniform_ring' }],
  ['UNI FAN SL V3 Wireless LCD', { lpf: 40, layout: 'uniform_ring' }],
  ['UNI FAN TL Wireless LCD', { lpf: 26, layout: 'uniform_ring' }],
  ['UNI FAN TL Wireless', { lpf: 26, layout: 'uniform_ring' }],
  ['UNI FAN SL-INF Wireless', { lpf: 44, layout: 'sl_inf44' }],
  ['UNI FAN CL Wireless', { lpf: 24, layout: 'uniform_ring' }],
  ['Wireless Device', { lpf: 20, layout: 'uniform_ring' }],
]);

const STRIP_KINDS = new Map<string, number>([
  ['Lancool 217 Wireless', 96],
  ['Universal Screen 8.8" Wireless', 88],
  ['Lancool V150 Wireless', 88],
]);

const AIO_KINDS = new Set(['HydroShift II LCD-C (Wireless)', 'HydroShift II LCD-S (Wireless)']);

/** Map a status device (`kind` display string + fan_count) to the Geometry
 * the daemon would compile effects against, or null when there is no layout
 * map (AIO, Strimer, unknown kind, or a fan device reporting zero fans). */
export function geometryForKind(kind: string, fanCount: number): Geometry | null {
  if (AIO_KINDS.has(kind)) return null;
  const total = STRIP_KINDS.get(kind);
  if (total !== undefined) return { type: 'strip', total };
  const fan = FAN_KINDS.get(kind);
  if (fan === undefined || fanCount <= 0) return null;
  return { type: 'fans', fan_count: fanCount, leds_per_fan: fan.lpf, layout: fan.layout };
}

/* ── Frame budget — mirror of effects_bridge::frame_budget ── */

/** Raw-byte animation budget from the Task 8 flash probe (effects_bridge.rs). */
export const RAW_BYTE_BUDGET = 28_000;

/** `clamp(28_000 / (leds × 3), 8, 96)` — integer division like the Rust.
 * Zero-LED geometry → 8, mirroring the daemon's guard. */
export function frameBudget(geom: Geometry): number {
  const leds = geometryLeds(geom);
  if (leds <= 0) return 8;
  const raw = Math.floor(RAW_BYTE_BUDGET / (leds * 3));
  return Math.min(96, Math.max(8, raw));
}

/* ── Store ── */

export interface StageSnapshot {
  /** MAC of the device on stage; null when nothing is selected. */
  selectedMac: string | null;
  /** The working (not yet applied) effect spec, always fully populated. */
  spec: EffectSpec;
}

export interface StageStore {
  getSnapshot(): StageSnapshot;
  subscribe(onChange: () => void): () => void;
  selectDevice(mac: string | null): void;
  /** Merge a partial edit into the working spec, clamped to daemon limits.
   * Notifies only when the clamped result actually differs. */
  setSpec(patch: Partial<EffectSpec>): void;
}

export function createStageStore(): StageStore {
  let snapshot: StageSnapshot = { selectedMac: null, spec: clampSpec(defaultSpec()) };
  const listeners = new Set<() => void>();

  function notify() {
    for (const cb of listeners) cb();
  }

  return {
    getSnapshot: () => snapshot,
    subscribe(onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },
    selectDevice(mac) {
      if (snapshot.selectedMac === mac) return;
      snapshot = { ...snapshot, selectedMac: mac };
      notify();
    },
    setSpec(patch) {
      const next = clampSpec({ ...snapshot.spec, ...patch });
      if (specsEqual(next, snapshot.spec)) return;
      snapshot = { ...snapshot, spec: next };
      notify();
    },
  };
}

/** App-wide singleton — the one working spec the stage and (C4) the effect
 * rail share. Framework-free; Lighting.tsx wraps it in `useStage()`. */
export const stageStore = createStageStore();
