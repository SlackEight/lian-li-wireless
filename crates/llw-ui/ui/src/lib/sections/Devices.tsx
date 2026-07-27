import { useEffect, useRef, useState, type CSSProperties } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useStatus } from '../stores/useStatus.js';
import { useBindFlow, bindFlow } from '../stores/useBindFlow.js';
import { toastStore } from '../stores/useToasts.js';
import {
  airRowKind,
  sliceToFanCount,
  type AirDeviceStatus,
  type AirRowKind,
  type DeviceStatus,
} from '../stores/status.js';
import { isActiveOp, type OpState } from '../stores/bindFlow.js';
import {
  setDeviceAllSlotsMb,
  setDeviceSoftwareSlots,
  SOFTWARE_DEFAULT_PCT,
  type CoolingConfig,
} from '../stores/coolingModel.js';
import ConfirmDialog from '../components/ConfirmDialog.js';

/* ── Config mirror — round-tripped verbatim; only `name` is ever mutated ── */

type Rgb = [number, number, number];

interface ConfigDevice {
  mac: string;
  name: string | null;
  color?: { rgb: Rgb; brightness: number } | null;
  effect?: { colors?: Rgb[] } | null;
  [key: string]: unknown; // slots etc. pass through untouched
}

interface DaemonConfig {
  schema_version: number;
  devices: ConfigDevice[];
  [key: string]: unknown; // curves/control/reliability/observation untouched
}

/** RGB tint for the ring: effect palette first (it overrides color), then
 * the static color, else empty (neutral ring). */
function devicePalette(cfg: ConfigDevice | undefined): Rgb[] {
  if (!cfg) return [];
  const colors = cfg.effect?.colors;
  if (colors && colors.length > 0) return colors;
  if (cfg.color) return [cfg.color.rgb];
  return [];
}

/** Static conic ring fill: device RGB tint with a soft bloom, or a quiet
 * neutral hairline gradient when the config carries no colors. */
function ringStyle(palette: Rgb[]): CSSProperties {
  if (palette.length === 0) {
    return {
      background:
        'conic-gradient(from 200deg, rgba(255, 255, 255, 0.16), rgba(255, 255, 255, 0.04) 60%, rgba(255, 255, 255, 0.10))',
    };
  }
  const stops = palette.slice(0, 4).map(([r, g, b]) => `rgb(${r}, ${g}, ${b})`);
  const [r, g, b] = palette[0];
  return {
    background: `conic-gradient(from 200deg, ${stops.join(', ')}, var(--surface) 60%)`,
    boxShadow: `0 0 18px rgba(${r}, ${g}, ${b}, 0.35), inset 0 0 8px rgba(0, 0, 20, 0.5)`,
  };
}

function fanLabel(count: number): string {
  return count === 1 ? '1 fan' : `${count} fans`;
}

/* ── Shared op-state fragments ── */

function OpProgress({ state }: { state: OpState }) {
  const label =
    state.phase === 'settling-retry'
      ? 'radio settling — retrying…'
      : state.phase === 'converging'
        ? 'converging…'
        : state.op === 'bind'
          ? 'linking…'
          : 'unlinking…';
  return (
    <span className="op-progress">
      <span className="acquiring-dot" aria-hidden="true"></span>
      {label}
    </span>
  );
}

/* ── Configured device card ── */

function DeviceName({
  name,
  fallback,
  onCommit,
}: {
  name: string | null;
  fallback: string;
  onCommit: (raw: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const cancelled = useRef(false);

  if (!editing) {
    return (
      <button type="button" className="device-name" title="rename" onClick={() => setEditing(true)}>
        {name?.trim() ? name : fallback}
      </button>
    );
  }
  return (
    <input
      className="device-name-input"
      defaultValue={name ?? ''}
      placeholder={fallback}
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

/** Speed-source values as the daemon reports them in `speed_source`. */
type SpeedSource = 'software' | 'motherboard';

function SpeedToggle({
  label,
  source,
  disabled,
  onSelect,
}: {
  label: string;
  source: string;
  disabled: boolean;
  onSelect: (next: SpeedSource) => void;
}) {
  const mb = source === 'motherboard';
  return (
    <div className="segmented device-speed" role="radiogroup" aria-label={`${label} speed source`}>
      <button
        type="button"
        role="radio"
        aria-checked={!mb}
        className={mb ? 'segment' : 'segment active'}
        disabled={disabled}
        onClick={() => {
          if (mb) onSelect('software');
        }}
      >
        Software
      </button>
      <button
        type="button"
        role="radio"
        aria-checked={mb}
        className={mb ? 'segment active' : 'segment'}
        disabled={disabled}
        onClick={() => {
          if (!mb) onSelect('motherboard');
        }}
      >
        Motherboard
      </button>
    </div>
  );
}

function ConfiguredCard({
  device,
  cfg,
  op,
  speedBusy,
  onRename,
  onSpeedSource,
  onAskUnbind,
}: {
  device: DeviceStatus;
  cfg: ConfigDevice | undefined;
  op: OpState | undefined;
  speedBusy: boolean;
  onRename: (mac: string, raw: string) => void;
  onSpeedSource: (device: DeviceStatus, next: SpeedSource) => void;
  onAskUnbind: (target: { mac: string; label: string; mb: boolean }) => void;
}) {
  const unbindOp = op?.op === 'unbind' ? op : undefined;
  const displayName = cfg?.name?.trim() ? cfg.name : device.kind;
  const rpms = sliceToFanCount(device.rpm, device.fan_count);
  const rpm = rpms.length > 0 ? Math.max(...rpms) : null;
  const mb = device.speed_source === 'motherboard';

  return (
    <div className="card device-card">
      <div className="device-card-row">
        <div className="device-ring" style={ringStyle(devicePalette(cfg))} aria-hidden="true">
          <div className="device-ring-inner">
            <span className="ring-rpm">{rpm ?? '—'}</span>
            <span className="ring-rpm-label">rpm</span>
          </div>
        </div>
        <div className="device-card-info">
          <DeviceName
            name={cfg?.name ?? null}
            fallback={device.kind}
            onCommit={(raw) => onRename(device.mac, raw)}
          />
          <div className="device-mac mac">{device.mac}</div>
          <div className="device-meta">
            {device.kind} · {fanLabel(device.fan_count)}
          </div>
        </div>
      </div>
      <div className="device-card-foot">
        {device.speed_source !== undefined && (
          // Hidden entirely on pre-M4f daemons (no speed_source in Status).
          <SpeedToggle
            label={displayName}
            source={device.speed_source}
            disabled={speedBusy || isActiveOp(op)}
            onSelect={(next) => onSpeedSource(device, next)}
          />
        )}
        {unbindOp && isActiveOp(unbindOp) ? (
          <OpProgress state={unbindOp} />
        ) : (
          <button
            type="button"
            className="unbind-btn"
            onClick={() => onAskUnbind({ mac: device.mac, label: displayName, mb })}
          >
            Unlink
          </button>
        )}
      </div>
    </div>
  );
}

/* ── Air rows (non-hidden classifications from airRowKind) ── */

function AirRow({
  entry,
  kind,
  op,
  onLink,
}: {
  entry: AirDeviceStatus;
  kind: AirRowKind;
  op: OpState | undefined;
  onLink: () => void;
}) {
  const foreign = kind === 'foreign';
  const bindOp = op?.op === 'bind' ? op : undefined;

  return (
    <div className={`air-row${foreign ? ' foreign' : ''}`}>
      <div className="air-id">
        <div className="air-kind">{entry.kind}</div>
        <div className="air-mac mac">{entry.mac}</div>
      </div>
      <div className="air-meta">
        ch {entry.channel} · {fanLabel(entry.fan_count)} · seen{' '}
        {entry.last_seen_s === 0 ? 'now' : `${entry.last_seen_s}s ago`}
      </div>
      {foreign && <span className="air-note">linked to another controller</span>}
      {kind === 'adoptable' && <span className="air-note">paired, not linked</span>}
      <div className="air-action">
        {bindOp && isActiveOp(bindOp) ? (
          <OpProgress state={bindOp} />
        ) : bindOp?.phase === 'done' ? (
          <span className="op-done">linked ✓</span>
        ) : (
          <button
            type="button"
            className="bind-btn bloom"
            disabled={foreign}
            title={foreign ? 'linked to another controller — release it there first' : undefined}
            onClick={onLink}
          >
            Link
          </button>
        )}
      </div>
    </div>
  );
}

/* ── Screen ── */

export default function Devices() {
  const { data } = useStatus();
  const flow = useBindFlow();
  const [config, setConfig] = useState<DaemonConfig | null>(null);
  const [unbindTarget, setUnbindTarget] = useState<{
    mac: string;
    label: string;
    mb: boolean;
  } | null>(null);
  const [speedBusyMac, setSpeedBusyMac] = useState<string | null>(null);

  // Names/tints come from the config; refetch whenever the configured set
  // changes (a link/unlink landed). Failures fall back to kind + neutral
  // rings silently — the unreachable banner already tells that story.
  const configuredMacs = (data?.devices ?? [])
    .map((d) => d.mac)
    .sort()
    .join(',');
  useEffect(() => {
    let stale = false;
    invoke('get_config')
      .then((cfg) => {
        if (!stale) setConfig(cfg as DaemonConfig);
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [configuredMacs]);

  const cfgByMac = new Map((config?.devices ?? []).map((d) => [d.mac, d]));
  const configuredMacSet = new Set((data?.devices ?? []).map((d) => d.mac));
  // Every air entry the screen shows: non-Ours as always, plus Ours devices
  // missing from config ("paired, not linked" — Link adopts them without RF).
  const airRows = (data?.air ?? [])
    .map((entry) => ({ entry, kind: airRowKind(entry, configuredMacSet) }))
    .filter((row) => row.kind !== 'hidden');

  // Rename: fetch a fresh config, mutate only this device's name, round-trip
  // the whole thing back (the daemon validates). Errors surface as toasts.
  async function commitRename(mac: string, raw: string) {
    const name = raw.trim() === '' ? null : raw.trim();
    try {
      const cfg = (await invoke('get_config')) as DaemonConfig;
      const dev = cfg.devices.find((d) => d.mac === mac);
      if (!dev) {
        toastStore.push('error', `${mac} is not in the daemon config`);
        return;
      }
      if ((dev.name ?? null) !== name) {
        dev.name = name;
        await invoke('set_config', { json: cfg });
      }
      setConfig(cfg);
    } catch (err) {
      toastStore.push('error', typeof err === 'string' ? err : String(err));
    }
  }

  // Speed source: same round-trip idiom — fresh config, all-slot mutation via
  // the pure helpers, immediate SetConfig. The daemon's next status poll
  // flips `speed_source`, which settles the segmented control.
  async function commitSpeedSource(device: DeviceStatus, next: SpeedSource) {
    if (speedBusyMac !== null) return;
    setSpeedBusyMac(device.mac);
    try {
      const cfg = (await invoke('get_config')) as CoolingConfig;
      const mutated =
        next === 'motherboard'
          ? setDeviceAllSlotsMb(cfg, device.mac)
          : setDeviceSoftwareSlots(cfg, device.mac, device.fan_count);
      if (mutated === cfg) {
        toastStore.push('error', `${device.mac} is not in the daemon config`);
        return;
      }
      await invoke('set_config', { json: mutated });
      setConfig(mutated as unknown as DaemonConfig);
      toastStore.push(
        'ok',
        next === 'motherboard'
          ? 'motherboard control — fans follow the header'
          : `software control at ${SOFTWARE_DEFAULT_PCT}% — refine in Cooling`,
      );
    } catch (err) {
      toastStore.push('error', typeof err === 'string' ? err : String(err));
    } finally {
      setSpeedBusyMac(null);
    }
  }

  function confirmUnbind() {
    if (!unbindTarget) return;
    bindFlow.start('unbind', unbindTarget.mac);
    setUnbindTarget(null);
  }

  return (
    <section className="section-content">
      <header className="section-header">
        <h1 className="section-title">Devices</h1>
        <p className="section-subtitle">Configured hardware and air-visible peripherals</p>
      </header>

      {data === null ? (
        <div className="card-muted placeholder-card">
          <span>Waiting for daemon data</span>
          <span className="hint">polling status…</span>
        </div>
      ) : (
        <>
          <h2 className="subsection-title">Configured</h2>
          {data.devices.length === 0 ? (
            <div className="card-muted placeholder-card">
              <span>No configured devices</span>
              <span className="hint">link one from the air list below</span>
            </div>
          ) : (
            <div className="devices-grid">
              {data.devices.map((device) => (
                <ConfiguredCard
                  key={device.mac}
                  device={device}
                  cfg={cfgByMac.get(device.mac)}
                  op={flow[device.mac]}
                  speedBusy={speedBusyMac !== null}
                  onRename={(mac, raw) => void commitRename(mac, raw)}
                  onSpeedSource={(dev, next) => void commitSpeedSource(dev, next)}
                  onAskUnbind={setUnbindTarget}
                />
              ))}
            </div>
          )}

          <h2 className="subsection-title">On air</h2>
          {airRows.length === 0 ? (
            <div className="card-muted placeholder-card">
              <span>Nothing else on air</span>
              <span className="hint">unlinked devices appear here when powered</span>
            </div>
          ) : (
            <div className="air-list">
              {airRows.map(({ entry, kind }) => (
                <AirRow
                  key={entry.mac}
                  entry={entry}
                  kind={kind}
                  op={flow[entry.mac]}
                  onLink={() => bindFlow.start('bind', entry.mac)}
                />
              ))}
            </div>
          )}
        </>
      )}

      {unbindTarget && (
        <ConfirmDialog
          title={`Unlink ${unbindTarget.label}?`}
          body={
            unbindTarget.mb
              ? 'This removes the device from the daemon config and releases it on air. The fans keep running under motherboard control from the header wire.'
              : 'This removes the device from the daemon config and releases it on air — it stays uncontrolled until linked again.'
          }
          confirmLabel="Unlink"
          onConfirm={confirmUnbind}
          onCancel={() => setUnbindTarget(null)}
        />
      )}
    </section>
  );
}
