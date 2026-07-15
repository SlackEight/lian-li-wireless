import {
  useMemo,
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import {
  type CurvePoint,
  TEMP_MIN,
  TEMP_MAX,
  DUTY_MIN,
  DUTY_MAX,
  movePoint,
  addPoint,
  removePoint,
  interpolate,
  clampTemp,
  clampDuty,
} from '../stores/curveModel.js';

interface Props {
  /** `[temp °C, duty %]` pairs — the config wire shape, any order. */
  points: CurvePoint[];
  /** Fired with a NEW sorted array on every edit (controlled component). */
  onChange: (points: CurvePoint[]) => void;
  /** Current sensor reading; null/undefined hides the live cursor entirely. */
  liveTemp?: number | null;
  disabled?: boolean;
}

/* ── ViewBox geometry (SVG user units; the svg stretches to its CSS box) ── */
const VB_W = 560;
const VB_H = 280;
const PAD_L = 36;
const PAD_R = 14;
const PAD_T = 16;
const PAD_B = 26;
const PLOT_W = VB_W - PAD_L - PAD_R;
const PLOT_H = VB_H - PAD_T - PAD_B;

const xOf = (t: number) => PAD_L + ((t - TEMP_MIN) / (TEMP_MAX - TEMP_MIN)) * PLOT_W;
const yOf = (d: number) => PAD_T + (1 - (d - DUTY_MIN) / (DUTY_MAX - DUTY_MIN)) * PLOT_H;
const tempAt = (vx: number) => TEMP_MIN + ((vx - PAD_L) / PLOT_W) * (TEMP_MAX - TEMP_MIN);
const dutyAt = (vy: number) => DUTY_MIN + (1 - (vy - PAD_T) / PLOT_H) * (DUTY_MAX - DUTY_MIN);
/** Trim path/attr numbers to 2 decimals. */
const fmt = (n: number) => Math.round(n * 100) / 100;

const X_GRID = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110];
const X_LABELS = [0, 20, 40, 60, 80, 100];
const Y_GRID = [25, 50, 75, 100];
const Y_LABELS = [0, 25, 50, 75, 100];

/**
 * SVG fan-curve editor. Controlled: all mutations flow through curveModel
 * ops and come back via `onChange`. Interactions: drag a point (pointer
 * capture), arrow keys nudge ±1 (shift = ±5), Delete/Backspace or
 * double-click removes (min-2 rule silently enforced by the model), click
 * on empty canvas adds a point.
 */
export default function CurveEditor({ points, onChange, liveTemp, disabled = false }: Props) {
  const svgRef = useRef<SVGSVGElement>(null);
  /** Index into `points` of the point being dragged (tracked across re-sorts). */
  const dragIdx = useRef<number | null>(null);
  /** Pointer-down position on the empty canvas — an add only fires for a true click. */
  const addDown = useRef<{ x: number; y: number } | null>(null);

  // Render order sorted by temp; `i` stays the index into the `points` prop
  // so model ops address the caller's array (config may store it unsorted).
  const ordered = useMemo(
    () => points.map((p, i) => ({ t: p[0], d: p[1], i })).sort((a, b) => a.t - b.t),
    [points],
  );

  function toTempDuty(e: { clientX: number; clientY: number }) {
    const svg = svgRef.current;
    if (!svg) return null;
    const rect = svg.getBoundingClientRect();
    // preserveAspectRatio="none" → each axis maps linearly onto the CSS box.
    const vx = ((e.clientX - rect.left) / rect.width) * VB_W;
    const vy = ((e.clientY - rect.top) / rect.height) * VB_H;
    return { temp: tempAt(vx), duty: dutyAt(vy) };
  }

  /* ── Point drag ── */

  function onPointDown(e: ReactPointerEvent<SVGCircleElement>, idx: number) {
    if (disabled) return;
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    dragIdx.current = idx;
  }

  function onPointMove(e: ReactPointerEvent<SVGCircleElement>) {
    if (dragIdx.current === null) return;
    const at = toTempDuty(e);
    if (!at) return;
    const next = movePoint(points, dragIdx.current, at.temp, at.duty);
    // The array re-sorts as the point crosses neighbours — follow it. The
    // moved point's stored value is exactly the clamped coordinates; with
    // exact duplicates any twin is visually identical, so first-match is fine.
    const ct = clampTemp(at.temp);
    const cd = clampDuty(at.duty);
    const ni = next.findIndex((p) => p[0] === ct && p[1] === cd);
    if (ni !== -1) dragIdx.current = ni;
    onChange(next);
  }

  function onPointUp(e: ReactPointerEvent<SVGCircleElement>) {
    if (dragIdx.current === null) return;
    dragIdx.current = null;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  }

  /* ── Keyboard nudge / remove ── */

  function onPointKey(e: ReactKeyboardEvent<SVGCircleElement>, idx: number) {
    if (disabled) return;
    const step = e.shiftKey ? 5 : 1;
    const [t, d] = points[idx];
    let next: CurvePoint[];
    switch (e.key) {
      case 'ArrowLeft':
        next = movePoint(points, idx, t - step, d);
        break;
      case 'ArrowRight':
        next = movePoint(points, idx, t + step, d);
        break;
      case 'ArrowUp':
        next = movePoint(points, idx, t, d + step);
        break;
      case 'ArrowDown':
        next = movePoint(points, idx, t, d - step);
        break;
      case 'Delete':
      case 'Backspace':
        next = removePoint(points, idx);
        break;
      default:
        return;
    }
    e.preventDefault();
    if (next !== points) onChange(next);
  }

  /* ── Add on empty-canvas click / remove on double-click ── */

  function onCanvasDown(e: ReactPointerEvent<SVGRectElement>) {
    if (disabled) return;
    addDown.current = { x: e.clientX, y: e.clientY };
  }

  function onCanvasUp(e: ReactPointerEvent<SVGRectElement>) {
    const down = addDown.current;
    addDown.current = null;
    if (disabled || !down) return;
    if (Math.hypot(e.clientX - down.x, e.clientY - down.y) > 4) return; // drag, not click
    const at = toTempDuty(e);
    if (at) onChange(addPoint(points, at.temp, at.duty));
  }

  function onPointDoubleClick(idx: number) {
    if (disabled) return;
    const next = removePoint(points, idx);
    if (next !== points) onChange(next); // min-2: silently keep the curve
  }

  /* ── Derived geometry ── */

  // The drawn path mirrors interpolate(): flat hold below the first point and
  // above the last, linear between.
  const pathD = useMemo(() => {
    if (ordered.length === 0) return null;
    const first = ordered[0];
    const last = ordered[ordered.length - 1];
    const mid = ordered.map((p) => `L ${fmt(xOf(p.t))} ${fmt(yOf(p.d))}`).join(' ');
    return `M ${fmt(xOf(TEMP_MIN))} ${fmt(yOf(first.d))} ${mid} L ${fmt(xOf(TEMP_MAX))} ${fmt(yOf(last.d))}`;
  }, [ordered]);
  const areaD = pathD
    ? `${pathD} L ${fmt(xOf(TEMP_MAX))} ${fmt(yOf(DUTY_MIN))} L ${fmt(xOf(TEMP_MIN))} ${fmt(yOf(DUTY_MIN))} Z`
    : null;

  const live =
    liveTemp !== null && liveTemp !== undefined && Number.isFinite(liveTemp)
      ? { temp: liveTemp, duty: interpolate(points, liveTemp) }
      : null;
  const liveX = live ? xOf(Math.min(TEMP_MAX, Math.max(TEMP_MIN, live.temp))) : 0;
  const liveY = live ? yOf(live.duty) : 0;
  const liveLabelLeft = liveX > VB_W - 96; // flip the label near the right edge
  const liveLabelY = Math.max(PAD_T + 8, liveY - 12);

  return (
    <div className={disabled ? 'curve-editor disabled' : 'curve-editor'}>
      <svg
        ref={svgRef}
        className="curve-editor-svg"
        viewBox={`0 0 ${VB_W} ${VB_H}`}
        preserveAspectRatio="none"
        role="group"
        aria-label="Fan curve editor"
      >
        {/* Grid */}
        {X_GRID.map((t) => (
          <line
            key={`gx${t}`}
            className="curve-grid-line"
            x1={xOf(t)}
            y1={PAD_T}
            x2={xOf(t)}
            y2={yOf(DUTY_MIN)}
          />
        ))}
        {Y_GRID.map((d) => (
          <line
            key={`gy${d}`}
            className="curve-grid-line"
            x1={PAD_L}
            y1={yOf(d)}
            x2={xOf(TEMP_MAX)}
            y2={yOf(d)}
          />
        ))}
        {/* Axes */}
        <line className="curve-axis-line" x1={PAD_L} y1={PAD_T} x2={PAD_L} y2={yOf(DUTY_MIN)} />
        <line
          className="curve-axis-line"
          x1={PAD_L}
          y1={yOf(DUTY_MIN)}
          x2={xOf(TEMP_MAX)}
          y2={yOf(DUTY_MIN)}
        />
        {/* Axis labels */}
        {X_LABELS.map((t) => (
          <text
            key={`lx${t}`}
            className="curve-axis-label"
            x={xOf(t)}
            y={VB_H - 8}
            textAnchor="middle"
          >
            {t}
          </text>
        ))}
        <text className="curve-axis-label" x={xOf(TEMP_MAX)} y={VB_H - 8} textAnchor="middle">
          °C
        </text>
        {Y_LABELS.map((d) => (
          <text
            key={`ly${d}`}
            className="curve-axis-label"
            x={PAD_L - 6}
            y={yOf(d) + 3}
            textAnchor="end"
          >
            {d}
          </text>
        ))}
        <text className="curve-axis-label" x={PAD_L - 6} y={PAD_T - 6} textAnchor="end">
          %
        </text>

        {/* Curve */}
        {areaD && <path className="curve-area" d={areaD} />}
        {pathD && <path className="curve-path" d={pathD} />}

        {/* Empty-canvas click target (under the points, over the curve) */}
        <rect
          className="curve-hit"
          x={PAD_L}
          y={PAD_T}
          width={PLOT_W}
          height={PLOT_H}
          onPointerDown={onCanvasDown}
          onPointerUp={onCanvasUp}
        />

        {/* Live temp cursor (the one saturated element) */}
        {live && (
          <g className="curve-live" pointerEvents="none">
            <line
              className="curve-live-line"
              x1={liveX}
              y1={PAD_T}
              x2={liveX}
              y2={yOf(DUTY_MIN)}
            />
            <circle className="curve-live-dot" cx={liveX} cy={liveY} r={3.5} />
            <text
              className="curve-live-label"
              x={liveLabelLeft ? liveX - 8 : liveX + 8}
              y={liveLabelY}
              textAnchor={liveLabelLeft ? 'end' : 'start'}
            >
              {Math.round(live.temp)} °C → {Math.round(live.duty)}%
            </text>
          </g>
        )}

        {/* Points */}
        {ordered.map((p) => (
          <circle
            key={p.i}
            className="curve-point"
            cx={xOf(p.t)}
            cy={yOf(p.d)}
            r={5.5}
            tabIndex={disabled ? -1 : 0}
            role="slider"
            aria-label={`Curve point at ${p.t} °C`}
            aria-valuemin={DUTY_MIN}
            aria-valuemax={DUTY_MAX}
            aria-valuenow={p.d}
            aria-valuetext={`${p.t} °C, ${p.d}%`}
            aria-disabled={disabled || undefined}
            onPointerDown={(e) => onPointDown(e, p.i)}
            onPointerMove={onPointMove}
            onPointerUp={onPointUp}
            onPointerCancel={onPointUp}
            onKeyDown={(e) => onPointKey(e, p.i)}
            onDoubleClick={() => onPointDoubleClick(p.i)}
          />
        ))}
      </svg>
    </div>
  );
}
