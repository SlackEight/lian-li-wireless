import { Fragment, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useStatus } from '../stores/useStatus.js';
import { toastStore } from '../stores/useToasts.js';
import { sliceToFanCount, type DeviceStatus } from '../stores/status.js';
import CurveEditor from '../components/CurveEditor.js';
import ConfirmDialog from '../components/ConfirmDialog.js';
import NamePromptDialog from '../components/NamePromptDialog.js';
import {
  type CoolingConfig,
  type ConfigCurve,
  type ConfigDevice,
  type SensorSpec,
  addCurve,
  curveReferences,
  deleteCurve,
  renameCurve,
  setCurvePoints,
  setCurveSensor,
  setSlot,
} from '../stores/coolingModel.js';
import type { CurvePoint } from '../stores/curveModel.js';

/* ── list_sensors reply mirror (llw-daemon's sensors::SensorInfo) ── */

interface SensorInfo {
  chip: string;
  label: string;
  /** Verbatim config `SensorSpec` — usable as a curve's sensor as-is. */
  spec: SensorSpec;
  /** Best-effort reading in °C; null on read failure. */
  current_c: number | null;
}

/** Stable identity for a sensor spec (sysfs names cannot contain newlines). */
function specKey(spec: SensorSpec): string {
  return `${spec.hwmon_name}\n${spec.input}`;
}

function sensorLabel(s: SensorInfo): string {
  const temp = s.current_c !== null ? ` (${s.current_c.toFixed(1)} °C)` : '';
  return `${s.chip} · ${s.label}${temp}`;
}

// PWM readbacks are raw 0–255 bytes on the wire; people read percentages.
function pwmPercent(raw: number): string {
  return `${Math.round((raw / 255) * 100)}%`;
}

function describeRefs(refs: { device: string; slot: number }[]): string {
  return refs.map((r) => `${r.device} fan ${r.slot + 1}`).join(', ');
}

/* In-theme name prompt: extracted to components/NamePromptDialog.tsx (C4). */

/* ── Curve list row (inline rename, Devices-style) ── */

function CurveName({
  name,
  onCommit,
}: {
  name: string;
  onCommit: (raw: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const cancelled = useRef(false);

  if (!editing) {
    return (
      <button
        type="button"
        className="curve-row-name"
        title="rename"
        onClick={() => setEditing(true)}
      >
        {name}
      </button>
    );
  }
  return (
    <input
      className="curve-row-name-input"
      defaultValue={name}
      autoFocus
      onKeyDown={(e) => {
        if (e.key === 'Enter') e.currentTarget.blur();
        if (e.key === 'Escape') {
          cancelled.current = true;
          setEditing(false);
        }
      }}
      onBlur={(e) => {
        if (cancelled.current) {
          cancelled.current = false;
          return;
        }
        setEditing(false);
        onCommit(e.currentTarget.value);
      }}
    />
  );
}

function CurveRow({
  curve,
  active,
  refCount,
  onSelect,
  onRename,
  onAskDelete,
}: {
  curve: ConfigCurve;
  active: boolean;
  refCount: number;
  onSelect: () => void;
  onRename: (raw: string) => void;
  onAskDelete: () => void;
}) {
  return (
    <div className={active ? 'curve-row selected' : 'curve-row'}>
      {active ? (
        // The selected row's name is the rename affordance (Devices idiom).
        <div className="curve-row-info">
          <CurveName name={curve.name} onCommit={onRename} />
          <span className="curve-row-meta">
            {curve.sensor.hwmon_name} · {refCount === 1 ? '1 slot' : `${refCount} slots`}
          </span>
        </div>
      ) : (
        <button type="button" className="curve-row-info as-button" onClick={onSelect}>
          <span className="curve-row-name-static">{curve.name}</span>
          <span className="curve-row-meta">
            {curve.sensor.hwmon_name} · {refCount === 1 ? '1 slot' : `${refCount} slots`}
          </span>
        </button>
      )}
      <button
        type="button"
        className="curve-delete-btn"
        title={`delete ${curve.name}`}
        aria-label={`delete curve ${curve.name}`}
        onClick={onAskDelete}
      >
        ✕
      </button>
    </div>
  );
}

/* ── Fixed-% input (local draft so the field can be cleared while typing) ── */

function PercentInput({
  value,
  disabled,
  onCommit,
}: {
  value: number;
  disabled: boolean;
  onCommit: (pct: number) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  return (
    <input
      type="number"
      className="slot-pct-input"
      min={0}
      max={100}
      step={1}
      disabled={disabled}
      aria-label="fixed speed percent"
      value={draft ?? String(value)}
      onChange={(e) => {
        setDraft(e.currentTarget.value);
        const v = e.currentTarget.valueAsNumber;
        if (Number.isFinite(v)) onCommit(v);
      }}
      onBlur={() => setDraft(null)}
    />
  );
}

/* ── Per-device slot assignment card ── */

// Slot-select values: curves are user-named, so namespace the options.
const FIXED_VALUE = 'f';
const curveValue = (name: string) => `c:${name}`;

function SlotCard({
  device,
  cfgDev,
  curveNames,
  disabled,
  onSetSlot,
}: {
  device: DeviceStatus;
  cfgDev: ConfigDevice;
  curveNames: string[];
  disabled: boolean;
  onSetSlot: (slotIdx: number, speed: number | string) => void;
}) {
  const displayName = cfgDev.name?.trim() ? cfgDev.name : device.kind;
  const rpms = sliceToFanCount(device.rpm, device.fan_count);

  return (
    <div className="card slot-card">
      <div className="slot-card-head">
        <div>
          <div className="device-kind">{displayName}</div>
          <div className="device-mac mac">{device.mac}</div>
        </div>
        <span className="slot-card-kind">{device.kind}</span>
      </div>

      {device.fan_count === 0 ? (
        <div className="slot-none">no fan slots</div>
      ) : (
        <div className="slot-grid">
          <span className="slot-cell head name">fan</span>
          <span className="slot-cell head mode">source</span>
          <span className="slot-cell head">value</span>
          <span className="slot-cell head">rpm</span>
          <span className="slot-cell head">pwm</span>
          {rpms.map((rpm, i) => {
            const slot = cfgDev.slots[i] ?? 0;
            const isCurve = typeof slot === 'string';
            // A reference to a curve deleted out from under us should still
            // render (the daemon would refuse the save) — keep it visible.
            const orphan = isCurve && !curveNames.includes(slot);
            return (
              <Fragment key={i}>
                <span className="slot-cell name">{i + 1}</span>
                <span className="slot-cell mode">
                  <select
                    className="slot-select"
                    disabled={disabled}
                    aria-label={`fan ${i + 1} speed source`}
                    value={isCurve ? curveValue(slot) : FIXED_VALUE}
                    onChange={(e) => {
                      const v = e.currentTarget.value;
                      if (v === FIXED_VALUE) {
                        // Start the fixed value at what the daemon currently
                        // commands, so saving does not jump the fan.
                        const desired = device.desired_pwm[i];
                        const pct = Number.isFinite(desired)
                          ? Math.round((desired / 255) * 100)
                          : 50;
                        onSetSlot(i, pct);
                      } else {
                        onSetSlot(i, v.slice(2));
                      }
                    }}
                  >
                    <option value={FIXED_VALUE}>fixed %</option>
                    {curveNames.map((name) => (
                      <option key={name} value={curveValue(name)}>
                        {name}
                      </option>
                    ))}
                    {orphan && <option value={curveValue(slot)}>{slot} (missing)</option>}
                  </select>
                </span>
                <span className="slot-cell">
                  {isCurve ? (
                    <span className="slot-curve-note">auto</span>
                  ) : (
                    <PercentInput
                      value={slot}
                      disabled={disabled}
                      onCommit={(pct) => onSetSlot(i, pct)}
                    />
                  )}
                </span>
                <span className="slot-cell num">{rpm}</span>
                <span className="slot-cell num">{pwmPercent(device.readback_pwm[i])}</span>
              </Fragment>
            );
          })}
        </div>
      )}
    </div>
  );
}

/* ── Screen ── */

export default function Cooling() {
  const { data, daemonReachable } = useStatus();
  const [saved, setSaved] = useState<CoolingConfig | null>(null);
  const [working, setWorking] = useState<CoolingConfig | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [sensors, setSensors] = useState<SensorInfo[]>([]);
  const [saving, setSaving] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);

  // Load config + sensor list once; retry on the next reachable flip if the
  // daemon was down at mount. Never refetch behind the user's back once
  // loaded — edits are local until Save/Discard.
  const loaded = useRef(false);
  useEffect(() => {
    if (loaded.current || !daemonReachable) return;
    let stale = false;
    invoke('get_config')
      .then((cfg) => {
        if (stale || loaded.current) return;
        loaded.current = true;
        setSaved(cfg as CoolingConfig);
        setWorking(cfg as CoolingConfig);
      })
      .catch(() => {}); // banner tells the unreachable story; retried on flip
    invoke('list_sensors')
      .then((d) => {
        if (!stale) setSensors((d as { sensors: SensorInfo[] }).sensors ?? []);
      })
      .catch(() => {}); // picker degrades to "(unavailable)" for the current sensor
    return () => {
      stale = true;
    };
  }, [daemonReachable]);

  const dirty = useMemo(
    () => saved !== null && working !== null && JSON.stringify(working) !== JSON.stringify(saved),
    [saved, working],
  );

  // Effective selection: the named curve, else the first one.
  const curve: ConfigCurve | null =
    (working && (working.curves.find((c) => c.name === selected) ?? working.curves[0])) ?? null;
  const curveNames = (working?.curves ?? []).map((c) => c.name);
  const liveTemp = curve
    ? ((data?.curves ?? []).find((c) => c.name === curve.name)?.sensor_c ?? null)
    : null;
  const sensorEnumerated =
    curve !== null && sensors.some((s) => specKey(s.spec) === specKey(curve.sensor));

  /* ── Local edits (dirty until Save) ── */

  function edit(next: CoolingConfig | null) {
    if (next !== null) setWorking(next);
  }

  function onPointsChange(points: CurvePoint[]) {
    if (working && curve) edit(setCurvePoints(working, curve.name, points));
  }

  function onSensorChange(key: string) {
    if (!working || !curve) return;
    const sensor = sensors.find((s) => specKey(s.spec) === key);
    if (sensor) edit(setCurveSensor(working, curve.name, sensor.spec));
  }

  function onRename(oldName: string, raw: string) {
    const name = raw.trim();
    if (!working || name === '' || name === oldName) return;
    const next = renameCurve(working, oldName, name);
    if (next === working) {
      toastStore.push('warn', `a curve named "${name}" already exists`);
      return;
    }
    setWorking(next);
    setSelected(name);
  }

  function onAdd(raw: string) {
    if (!working) return;
    const name = raw.trim();
    // New curves start on the first enumerated sensor; with no enumeration
    // the placeholder spec reads as "(unavailable)" until rebound.
    const sensor = sensors[0]?.spec ?? { hwmon_name: '', input: '' };
    const next = addCurve(working, name, sensor);
    if (next === working) {
      toastStore.push('warn', `a curve named "${name}" already exists`);
      return;
    }
    setWorking(next);
    setSelected(name);
    setAddOpen(false);
  }

  function onAskDelete(name: string) {
    if (!working) return;
    const refs = curveReferences(working, name);
    if (refs.length > 0) {
      toastStore.push(
        'warn',
        `"${name}" is still assigned to ${describeRefs(refs)} — reassign those slots first`,
      );
      return;
    }
    setDeleteTarget(name);
  }

  function onConfirmDelete() {
    if (working && deleteTarget) {
      const res = deleteCurve(working, deleteTarget);
      if (res.ok) {
        setWorking(res.config);
        if (selected === deleteTarget) setSelected(null);
      } else {
        // A slot was reassigned to it between the guard and the confirm.
        toastStore.push('warn', `"${deleteTarget}" is still assigned to ${describeRefs(res.blockedBy)}`);
      }
    }
    setDeleteTarget(null);
  }

  function onSetSlot(mac: string, slotIdx: number, speed: number | string) {
    if (working) edit(setSlot(working, mac, slotIdx, speed));
  }

  /* ── Save / Discard (the explicit Apply pattern — SetConfig hits fan
        control immediately, so nothing auto-saves) ── */

  async function save() {
    if (!working || saving) return;
    setSaving(true);
    try {
      await invoke('set_config', { json: working });
      setSaved(working); // exactly what was sent; later edits stay dirty
    } catch (err) {
      toastStore.push('error', typeof err === 'string' ? err : String(err));
    } finally {
      setSaving(false);
    }
  }

  async function discard() {
    if (saving) return;
    try {
      const cfg = (await invoke('get_config')) as CoolingConfig;
      setSaved(cfg);
      setWorking(cfg);
    } catch (err) {
      toastStore.push('error', typeof err === 'string' ? err : String(err));
    }
  }

  return (
    <section className="section-content">
      <header className="section-header cooling-header">
        <div>
          <h1 className="section-title">Cooling</h1>
          <p className="section-subtitle">Fan curves, PWM targets, and thermal zones</p>
        </div>
        {(dirty || saving) && (
          <div className="save-bar">
            <button type="button" className="btn-quiet" disabled={saving} onClick={() => void discard()}>
              Discard
            </button>
            <button type="button" className="btn-accent bloom" disabled={saving} onClick={() => void save()}>
              {saving ? 'Saving…' : 'Save'}
            </button>
          </div>
        )}
      </header>

      {working === null ? (
        <div className="card-muted placeholder-card">
          <span>Waiting for daemon config</span>
          <span className="hint">fetching…</span>
        </div>
      ) : (
        <>
          <div className="cooling-layout">
            <aside className="card cooling-curves">
              <div className="cooling-curves-head">
                <span className="card-title">Curves</span>
                <button type="button" className="curve-add-btn" onClick={() => setAddOpen(true)}>
                  + add
                </button>
              </div>
              {working.curves.length === 0 ? (
                <div className="cooling-empty">
                  <span>No curves yet</span>
                  <span className="hint">add one to drive fans by temperature</span>
                </div>
              ) : (
                <div className="curve-list">
                  {working.curves.map((c) => (
                    <CurveRow
                      key={c.name}
                      curve={c}
                      active={curve?.name === c.name}
                      refCount={curveReferences(working, c.name).length}
                      onSelect={() => setSelected(c.name)}
                      onRename={(raw) => onRename(c.name, raw)}
                      onAskDelete={() => onAskDelete(c.name)}
                    />
                  ))}
                </div>
              )}
            </aside>

            <div className="card cooling-editor">
              {curve === null ? (
                <div className="cooling-empty tall">
                  <span>No curve selected</span>
                  <span className="hint">add a curve to start editing</span>
                </div>
              ) : (
                <>
                  <div className="cooling-editor-head">
                    <span className="cooling-editor-title">{curve.name}</span>
                    <label className="sensor-label">
                      sensor
                      <select
                        className="sensor-select"
                        disabled={saving}
                        value={specKey(curve.sensor)}
                        onChange={(e) => onSensorChange(e.currentTarget.value)}
                      >
                        {!sensorEnumerated && (
                          <option value={specKey(curve.sensor)}>
                            {curve.sensor.hwmon_name || '(none)'} · {curve.sensor.input || '—'}{' '}
                            (unavailable)
                          </option>
                        )}
                        {sensors.map((s, i) => (
                          <option key={`${specKey(s.spec)}#${i}`} value={specKey(s.spec)}>
                            {sensorLabel(s)}
                          </option>
                        ))}
                      </select>
                    </label>
                  </div>
                  <CurveEditor
                    points={curve.points}
                    onChange={onPointsChange}
                    liveTemp={liveTemp}
                    disabled={saving}
                  />
                </>
              )}
            </div>
          </div>

          <h2 className="subsection-title">Fan slots</h2>
          {data === null ? (
            <div className="card-muted placeholder-card">
              <span>Waiting for daemon data</span>
              <span className="hint">polling status…</span>
            </div>
          ) : data.devices.length === 0 ? (
            <div className="card-muted placeholder-card">
              <span>No configured devices</span>
              <span className="hint">bind one from Devices</span>
            </div>
          ) : (
            <div className="slot-cards">
              {data.devices.map((device) => {
                const cfgDev = working.devices.find((d) => d.mac === device.mac);
                if (!cfgDev) return null; // freshly bound mid-session — Discard reloads
                return (
                  <SlotCard
                    key={device.mac}
                    device={device}
                    cfgDev={cfgDev}
                    curveNames={curveNames}
                    disabled={saving}
                    onSetSlot={(slotIdx, speed) => onSetSlot(device.mac, slotIdx, speed)}
                  />
                );
              })}
            </div>
          )}
        </>
      )}

      {addOpen && (
        <NamePromptDialog
          title="Add curve"
          confirmLabel="Add"
          placeholder="curve name"
          onConfirm={onAdd}
          onCancel={() => setAddOpen(false)}
        />
      )}

      {deleteTarget && (
        <ConfirmDialog
          title={`Delete curve "${deleteTarget}"?`}
          body="The curve is removed from the working config. Nothing changes on the hardware until you Save."
          confirmLabel="Delete"
          onConfirm={onConfirmDelete}
          onCancel={() => setDeleteTarget(null)}
        />
      )}
    </section>
  );
}
