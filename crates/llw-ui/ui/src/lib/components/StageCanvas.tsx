/// <reference types="vite/client" />
/**
 * StageCanvas — the Lighting stage hero: a devicePixelRatio-aware canvas
 * playing a WASM-rendered animation for the current spec/geometry.
 *
 * Compile-once-play-frames, exactly like the hardware: on every spec or
 * geometry change the component calls `render_animation_json` ONCE with the
 * device's hardware frame budget, then a requestAnimationFrame loop steps
 * through the returned frames at `interval_ms` (time-accumulated — never
 * assumes 60 fps). LEDs sit at their real `led_layout_json` positions, one
 * cluster per fan laid out horizontally; strips run edge to edge.
 *
 * Graceful degradation: the wasm-pkg is a build artifact (`npm run
 * build:wasm`, gitignored). It is imported through `import.meta.glob`, which
 * resolves to an empty map when the pkg is absent — so `vite build` succeeds
 * without it (a plain dynamic-import literal would fail at bundle time) and
 * the component renders an on-theme placeholder instead of crashing.
 */
import { useEffect, useRef, useState } from 'react';
import { frameBudget, type EffectSpec, type Geometry } from '../stores/stage.js';

/* ── Optional WASM module (memoised module-level) ── */

interface WasmModule {
  /** wasm-bindgen init: fetches + instantiates the .wasm (idempotent). */
  default(): Promise<unknown>;
  render_animation_json(specJson: string, geometryJson: string, frames: number): string;
  led_layout_json(geometryJson: string): string;
}

const WASM_PKG_PATH = '../wasm-pkg/llw_effects_wasm.js';
// Build-time optional include: {} when the pkg has not been built. When it
// has, Vite code-splits the glue JS and emits the .wasm as an asset via the
// glue's `new URL('…_bg.wasm', import.meta.url)`.
const wasmModules = import.meta.glob('../wasm-pkg/llw_effects_wasm.js');

/** True when the preview engine was present at build/dev-serve time. */
export const wasmAvailable = WASM_PKG_PATH in wasmModules;

let wasmPromise: Promise<WasmModule> | null = null;

function loadWasm(): Promise<WasmModule> {
  if (wasmPromise === null) {
    const loader = wasmModules[WASM_PKG_PATH];
    wasmPromise = loader
      ? loader().then(async (m) => {
          const mod = m as WasmModule;
          await mod.default();
          return mod;
        })
      : Promise.reject(new Error('preview engine not built'));
    wasmPromise.catch(() => {}); // parked rejection must not be "unhandled"
  }
  return wasmPromise;
}

/* ── Wire shapes (llw-effects-wasm doc comments) ── */

interface LedPoint {
  fan: number;
  /** Unit-circle coords, y-UP (angle 0 = top, clockwise) — flip y to draw. */
  x: number;
  y: number;
}

interface RenderResult {
  frames: number;
  interval_ms: number;
  leds: number;
  /** Flat frame-major bytes: rgb[(frame*leds + led)*3 + channel]. */
  rgb: number[];
}

interface Animation {
  rgb: Uint8Array;
  frames: number;
  intervalMs: number;
  leds: number;
  points: LedPoint[];
  fanCount: number;
  strip: boolean;
}

/* ── Drawing ── */

const PAD = 18; // css px breathing room around the clusters

function draw(ctx: CanvasRenderingContext2D, anim: Animation, frameIdx: number, dpr: number) {
  const w = ctx.canvas.width / dpr;
  const h = ctx.canvas.height / dpr;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const { points, leds, rgb, fanCount, strip } = anim;
  const base = frameIdx * leds * 3;
  const cy = h / 2;

  // Cluster metrics: fans get a circle each in a horizontal row; strips span
  // the full width on the midline.
  const cellW = w / fanCount;
  const ringR = Math.max(8, Math.min(cellW / 2, h / 2) - PAD);
  const core = strip
    ? Math.max(1.5, Math.min(3.5, (w - 2 * PAD) / Math.max(1, leds) / 2.2))
    : Math.max(1.6, Math.min(4, ringR * 0.055));

  const px = (p: LedPoint): [number, number] => {
    if (strip) return [PAD + ((p.x + 1) / 2) * (w - 2 * PAD), cy];
    // y-up unit coords → canvas y grows downward, hence cy − y·r.
    return [cellW * (p.fan + 0.5) + p.x * ringR, cy - p.y * ringR];
  };

  // Pass 1 — unlit sockets, so geometry reads even where the effect is dark.
  ctx.globalCompositeOperation = 'source-over';
  ctx.fillStyle = 'rgba(255, 255, 255, 0.06)';
  ctx.beginPath();
  for (let i = 0; i < points.length && i < leds; i++) {
    const [x, y] = px(points[i]);
    ctx.moveTo(x + core, y);
    ctx.arc(x, y, core, 0, 2 * Math.PI);
  }
  ctx.fill();

  // Pass 2 — additive glow + core per lit LED. Two plain arc fills per dot
  // (no shadowBlur, no per-dot gradients) keeps 132 LEDs well above 30 fps
  // even on webkit2gtk. Dot alpha scales with the rendered intensity, which
  // already carries the spec's ×(brightness/4) post-scale.
  ctx.globalCompositeOperation = 'lighter';
  for (let i = 0; i < points.length && i < leds; i++) {
    const r = rgb[base + i * 3];
    const g = rgb[base + i * 3 + 1];
    const b = rgb[base + i * 3 + 2];
    const intensity = Math.max(r, g, b) / 255;
    if (intensity <= 0) continue;
    const [x, y] = px(points[i]);
    // Soft bloom halo.
    ctx.fillStyle = `rgba(${r}, ${g}, ${b}, ${(0.22 * intensity).toFixed(3)})`;
    ctx.beginPath();
    ctx.arc(x, y, core * 3.1, 0, 2 * Math.PI);
    ctx.fill();
    // Bright core.
    ctx.fillStyle = `rgba(${r}, ${g}, ${b}, ${(0.35 + 0.65 * intensity).toFixed(3)})`;
    ctx.beginPath();
    ctx.arc(x, y, core, 0, 2 * Math.PI);
    ctx.fill();
  }
  ctx.globalCompositeOperation = 'source-over';
}

/* ── Component ── */

export interface StageCanvasProps {
  geometry: Geometry;
  spec: EffectSpec;
  /** CSS pixel height of the stage area (width tracks the container). */
  height?: number;
}

export default function StageCanvas({ geometry, spec, height = 280 }: StageCanvasProps) {
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [engineError, setEngineError] = useState<string | null>(null);

  // Value-keyed deps: parents may rebuild spec/geometry objects every render
  // (status polls each second) — only a real change should re-render frames.
  const geomJson = JSON.stringify(geometry);
  const specJson = JSON.stringify(spec);

  useEffect(() => {
    if (!wasmAvailable) return;
    const wrap = wrapRef.current;
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext('2d');
    if (!wrap || !canvas || !ctx) return;

    let cancelled = false;
    let raf = 0;
    let anim: Animation | null = null;
    let frameIdx = 0;
    let lastTs: number | null = null;
    let acc = 0;
    let needsDraw = true;

    function resize() {
      const dpr = window.devicePixelRatio || 1;
      const bw = Math.max(1, Math.round(wrap!.clientWidth * dpr));
      const bh = Math.max(1, Math.round(wrap!.clientHeight * dpr));
      if (canvas!.width !== bw || canvas!.height !== bh) {
        canvas!.width = bw;
        canvas!.height = bh;
      }
      needsDraw = true;
    }

    function tick(ts: number) {
      raf = requestAnimationFrame(tick);
      if (anim === null) return;
      // Accumulate real elapsed time and step whole frames — correct at any
      // display refresh rate, and immune to rAF jitter.
      if (lastTs !== null) acc += ts - lastTs;
      lastTs = ts;
      const steps = Math.floor(acc / anim.intervalMs);
      if (steps > 0) {
        acc -= steps * anim.intervalMs;
        frameIdx = (frameIdx + steps) % anim.frames;
        needsDraw = true;
      }
      if (needsDraw) {
        needsDraw = false;
        draw(ctx!, anim, frameIdx, window.devicePixelRatio || 1);
      }
    }

    function startRaf() {
      // anim null = frames not ready (or engine failed) — nothing to play.
      if (anim !== null && raf === 0 && !document.hidden) {
        lastTs = null; // don't fast-forward across the paused gap
        raf = requestAnimationFrame(tick);
      }
    }

    function stopRaf() {
      if (raf !== 0) {
        cancelAnimationFrame(raf);
        raf = 0;
      }
    }

    function onVisibility() {
      if (document.hidden) stopRaf();
      else startRaf();
    }

    const ro = new ResizeObserver(resize);
    ro.observe(wrap);
    resize();
    document.addEventListener('visibilitychange', onVisibility);

    loadWasm()
      .then((wasm) => {
        if (cancelled) return;
        // The cold boundary: one layout call + ONE full-animation render per
        // spec/geometry change, at the same frame budget the daemon uploads.
        const points = JSON.parse(wasm.led_layout_json(geomJson)) as LedPoint[];
        const res = JSON.parse(
          wasm.render_animation_json(specJson, geomJson, frameBudget(geometry)),
        ) as RenderResult;
        anim = {
          rgb: Uint8Array.from(res.rgb),
          frames: Math.max(1, res.frames),
          intervalMs: Math.max(1, res.interval_ms),
          leds: res.leds,
          points,
          fanCount: geometry.type === 'fans' ? Math.max(1, geometry.fan_count) : 1,
          strip: geometry.type === 'strip',
        };
        frameIdx = 0;
        acc = 0;
        needsDraw = true;
        setEngineError(null);
        startRaf();
      })
      .catch((err: unknown) => {
        if (!cancelled) setEngineError(err instanceof Error ? err.message : String(err));
      });

    return () => {
      cancelled = true;
      stopRaf();
      ro.disconnect();
      document.removeEventListener('visibilitychange', onVisibility);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- geometry/spec are
    // represented by their JSON forms; object identity is deliberately ignored.
  }, [geomJson, specJson, height]);

  if (!wasmAvailable) {
    return (
      <div className="stage-canvas stage-canvas-fallback" style={{ height }}>
        <span>preview engine not built</span>
        <span className="stage-fallback-hint">npm run build:wasm</span>
      </div>
    );
  }

  if (engineError !== null) {
    return (
      <div className="stage-canvas stage-canvas-fallback" style={{ height }}>
        <span>preview engine failed</span>
        <span className="stage-fallback-hint">{engineError}</span>
      </div>
    );
  }

  return (
    <div ref={wrapRef} className="stage-canvas" style={{ height }}>
      <canvas ref={canvasRef} className="stage-canvas-el" aria-label="effect preview" />
    </div>
  );
}
