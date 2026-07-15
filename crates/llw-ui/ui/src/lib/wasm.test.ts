// Native ↔ WASM parity for the llw-effects-wasm bridge.
//
// Goldens (src/lib/wasm-goldens/goldens.json, committed) are rendered NATIVELY
// by `cargo test -p llw-effects-wasm generate_goldens -- --ignored` through the
// exact code path the WASM exports use. This suite loads the wasm-pack build
// (`npm run build:wasm` → src/lib/wasm-pkg, gitignored) and asserts the WASM
// output is byte-identical.
//
// The wasm-pack `--target web` output works in node too: we bypass its
// fetch-based loader by reading the .wasm bytes ourselves and calling
// `initSync({ module: bytes })`.
//
// If the pkg has not been built, the whole suite skips (green `npm run test`
// for developers without the wasm toolchain).

import { describe, it, expect, beforeAll } from 'vitest';
import { existsSync, readFileSync } from 'node:fs';
import { pathToFileURL, fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const pkgJs = path.join(here, 'wasm-pkg', 'llw_effects_wasm.js');
const pkgWasm = path.join(here, 'wasm-pkg', 'llw_effects_wasm_bg.wasm');
const goldensPath = path.join(here, 'wasm-goldens', 'goldens.json');

const wasmBuilt = existsSync(pkgJs) && existsSync(pkgWasm);
if (!wasmBuilt) {
  // eslint-disable-next-line no-console
  console.warn(
    'wasm.test.ts: wasm pkg not found — parity suite SKIPPED. ' +
      'Run `npm run build:wasm` (needs wasm-pack + rust wasm32 target) to enable it.',
  );
}

interface WasmModule {
  initSync(opts: { module: BufferSource }): unknown;
  render_animation_json(specJson: string, geometryJson: string, frames: number): string;
  led_layout_json(geometryJson: string): string;
}

interface GoldenExpect {
  frames: number;
  interval_ms: number;
  leds: number;
  first_frame: number[];
  middle_index: number;
  middle_frame: number[];
}

interface GoldenCase {
  name: string;
  spec: unknown;
  geometry: unknown;
  frames: number;
  expect: GoldenExpect;
}

interface GoldensDoc {
  cases: GoldenCase[];
}

interface RenderResult {
  frames: number;
  interval_ms: number;
  leds: number;
  rgb: number[];
}

interface LedPoint {
  fan: number;
  x: number;
  y: number;
}

const goldens = JSON.parse(readFileSync(goldensPath, 'utf8')) as GoldensDoc;

describe.skipIf(!wasmBuilt)(
  'wasm effects bridge — native parity (skipped unless `npm run build:wasm` has run)',
  () => {
    let wasm: WasmModule;

    beforeAll(async () => {
      wasm = (await import(/* @vite-ignore */ pathToFileURL(pkgJs).href)) as unknown as WasmModule;
      wasm.initSync({ module: readFileSync(pkgWasm) });
    });

    it('goldens fixture has the expected 3 cases', () => {
      expect(goldens.cases.map((c) => c.name)).toEqual([
        'ripple-sl_inf44-3x44-70f',
        'rainbow-reverse-sl_inf44-3x44-70f',
        'breathing-uniform_ring-2x16-96f',
      ]);
    });

    for (const c of goldens.cases) {
      it(`render_animation_json: ${c.name} is byte-identical to native`, () => {
        const out = JSON.parse(
          wasm.render_animation_json(JSON.stringify(c.spec), JSON.stringify(c.geometry), c.frames),
        ) as RenderResult;

        expect(out.frames).toBe(c.expect.frames);
        expect(out.interval_ms).toBe(c.expect.interval_ms);
        expect(out.leds).toBe(c.expect.leds);
        expect(out.rgb.length).toBe(c.expect.frames * c.expect.leds * 3);

        const frameLen = c.expect.leds * 3;
        // Byte-exact: first frame and middle frame vs the native render.
        expect(out.rgb.slice(0, frameLen)).toEqual(c.expect.first_frame);
        const mid = c.expect.middle_index * frameLen;
        expect(out.rgb.slice(mid, mid + frameLen)).toEqual(c.expect.middle_frame);
      });
    }

    it('render_animation_json: serde parse error surfaces verbatim', () => {
      let thrown: unknown;
      try {
        wasm.render_animation_json(
          '{"kind":"frobnicate"}',
          '{"type":"fans","fan_count":3,"leds_per_fan":44,"layout":"sl_inf44"}',
          8,
        );
      } catch (e) {
        thrown = e;
      }
      expect(String(thrown)).toMatch(/invalid effect spec:.*frobnicate/);
    });

    it('led_layout_json: SL-INF 3×44 cluster shape and convention', () => {
      const pts = JSON.parse(
        wasm.led_layout_json('{"type":"fans","fan_count":3,"leds_per_fan":44,"layout":"sl_inf44"}'),
      ) as LedPoint[];

      expect(pts.length).toBe(132);
      expect(pts[0].fan).toBe(0);
      expect(pts[44].fan).toBe(1);
      expect(pts[131].fan).toBe(2);

      // LED 0 = inner ring start: angle 0.75 (left-middle), radius 0.7/1.15,
      // y-up coords → (−0.6087, 0). Pins the documented convention.
      expect(pts[0].x).toBeCloseTo(-0.7 / 1.15, 5);
      expect(pts[0].y).toBeCloseTo(0, 5);
      // LED 1 winds clockwise → upper-left quadrant.
      expect(pts[1].x).toBeLessThan(0);
      expect(pts[1].y).toBeGreaterThan(0);
      // Everything fits the unit circle (radii normalised by 1.15).
      for (const p of pts) {
        expect(Math.hypot(p.x, p.y)).toBeLessThanOrEqual(1.0 + 1e-6);
      }
    });
  },
);
