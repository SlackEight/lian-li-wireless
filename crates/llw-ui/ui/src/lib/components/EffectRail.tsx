/**
 * Effect rail (M4c Task C4): the Stage's right column — effect kind list,
 * palette editor, speed/direction/brightness, and the Apply button. Edits go
 * to the stage store (instant local canvas preview); Apply goes to the daemon
 * through the apply flow, which tracks the RF quiet window via rgb_in_sync.
 */
import { useRef } from 'react';
import {
  stageStore,
  specsEqual,
  EFFECT_KINDS,
  MAX_PALETTE,
  type EffectKind,
  type Rgb,
} from '../stores/stage.js';
import { useStage } from '../stores/useStage.js';
import { applyFlow, useApplyFlow } from '../stores/useApplyFlow.js';
import { isActiveApply } from '../stores/applyFlow.js';

/** 'color-cycle' → "Color cycle" */
function prettyKind(kind: EffectKind): string {
  const words = kind.replace(/-/g, ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

function rgbToHex([r, g, b]: Rgb): string {
  const h = (v: number) => v.toString(16).padStart(2, '0');
  return `#${h(r)}${h(g)}${h(b)}`;
}

function hexToRgb(hex: string): Rgb {
  return [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16),
  ];
}

/** A palette swatch backed by a hidden native color input. */
function Swatch({
  color,
  onChange,
  onRemove,
}: {
  color: Rgb;
  onChange: (c: Rgb) => void;
  onRemove: () => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  return (
    <span className="swatch-wrap">
      <button
        type="button"
        className="swatch"
        style={{ background: rgbToHex(color) }}
        title={`${rgbToHex(color)} — click to edit`}
        onClick={() => inputRef.current?.click()}
      />
      <input
        ref={inputRef}
        type="color"
        className="swatch-input"
        value={rgbToHex(color)}
        onChange={(e) => onChange(hexToRgb(e.currentTarget.value))}
        tabIndex={-1}
        aria-label="edit color"
      />
      <button
        type="button"
        className="swatch-remove"
        title="remove color"
        aria-label="remove color"
        onClick={onRemove}
      >
        ✕
      </button>
    </span>
  );
}

function Segmented<T extends string | number>({
  label,
  options,
  value,
  onSelect,
  disabled,
}: {
  label: string;
  options: { value: T; label: string }[];
  value: T;
  onSelect: (v: T) => void;
  disabled: boolean;
}) {
  return (
    <div className="rail-field">
      <span className="rail-label">{label}</span>
      <div className="segmented" role="radiogroup" aria-label={label}>
        {options.map((opt) => (
          <button
            key={String(opt.value)}
            type="button"
            role="radio"
            aria-checked={value === opt.value}
            className={value === opt.value ? 'segment active' : 'segment'}
            disabled={disabled}
            onClick={() => onSelect(opt.value)}
          >
            {opt.label}
          </button>
        ))}
      </div>
    </div>
  );
}

export default function EffectRail({ mac }: { mac: string | null }) {
  const { spec } = useStage();
  const flow = useApplyFlow();

  const state = mac !== null ? flow[mac] : undefined;
  const busy = isActiveApply(state);
  const last = mac !== null ? applyFlow.lastApplied(mac) : null;
  const dirty = last === null || !specsEqual(spec, last);

  const applyLabel =
    state?.phase === 'applying'
      ? 'applying…'
      : state?.phase === 'settling'
        ? 'RF quiet window…'
        : state?.phase === 'done'
          ? 'applied ✓'
          : 'Apply';

  return (
    <div className="effect-rail">
      <div className="rail-field">
        <span className="rail-label">Effect</span>
        <div className="rail-kinds">
          {EFFECT_KINDS.map((kind) => (
            <button
              key={kind}
              type="button"
              className={spec.kind === kind ? 'rail-kind active' : 'rail-kind'}
              disabled={busy}
              onClick={() => stageStore.setSpec({ kind })}
            >
              {prettyKind(kind)}
            </button>
          ))}
        </div>
      </div>

      <div className="rail-field">
        <span className="rail-label">Palette</span>
        <div className="swatch-row">
          {spec.colors.map((color, i) => (
            <Swatch
              key={i}
              color={color}
              onChange={(c) =>
                stageStore.setSpec({ colors: spec.colors.map((old, j) => (j === i ? c : old)) })
              }
              onRemove={() =>
                stageStore.setSpec({ colors: spec.colors.filter((_, j) => j !== i) })
              }
            />
          ))}
          {spec.colors.length < MAX_PALETTE && (
            <button
              type="button"
              className="swatch-add"
              title="add color"
              disabled={busy}
              onClick={() =>
                stageStore.setSpec({
                  colors: [...spec.colors, spec.colors[spec.colors.length - 1] ?? [136, 0, 255]],
                })
              }
            >
              +
            </button>
          )}
        </div>
        {spec.colors.length === 0 && (
          <span className="rail-hint">no palette — the effect uses its built-in colors</span>
        )}
      </div>

      <Segmented
        label="Speed"
        options={[1, 2, 3, 4, 5].map((v) => ({ value: v, label: String(v) }))}
        value={spec.speed}
        onSelect={(speed) => stageStore.setSpec({ speed })}
        disabled={busy}
      />

      <Segmented
        label="Direction"
        options={[
          { value: 'forward' as const, label: 'Forward' },
          { value: 'reverse' as const, label: 'Reverse' },
        ]}
        value={spec.direction}
        onSelect={(direction) => stageStore.setSpec({ direction })}
        disabled={busy}
      />

      <Segmented
        label="Brightness"
        options={[
          { value: 0, label: 'off' },
          ...[1, 2, 3, 4].map((v) => ({ value: v, label: String(v) })),
        ]}
        value={spec.brightness}
        onSelect={(brightness) => stageStore.setSpec({ brightness })}
        disabled={busy}
      />

      <button
        type="button"
        className={dirty && !busy && state?.phase !== 'done' ? 'apply-btn bloom dirty' : 'apply-btn'}
        disabled={mac === null || busy || (!dirty && state?.phase !== 'done')}
        onClick={() => mac !== null && applyFlow.start(mac, spec)}
      >
        {applyLabel}
      </button>
    </div>
  );
}
