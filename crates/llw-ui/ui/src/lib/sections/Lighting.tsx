/**
 * Lighting stage (M4c Task C3): device picker pills + the live WASM-rendered
 * StageCanvas hero. The working effect spec lives in the stage store
 * singleton (stores/stage.ts) so the C4 effect rail edits the same spec this
 * canvas previews; the right rail column below is C4's mount point.
 */
import { useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useStatus } from '../stores/useStatus.js';
import { stageStore, geometryForKind, type StageSnapshot } from '../stores/stage.js';
import StageCanvas from '../components/StageCanvas.js';

/** React binding for the stage store singleton (useStatus idiom, minus the
 * lifecycle — the store is passive). Exported for the C4 effect rail. */
export function useStage(): StageSnapshot {
  return useSyncExternalStore(stageStore.subscribe, stageStore.getSnapshot);
}

/* ── Config mirror — read-only here, names only (Devices.tsx idiom) ── */

interface ConfigDevice {
  mac: string;
  name: string | null;
  [key: string]: unknown;
}

interface DaemonConfig {
  devices: ConfigDevice[];
  [key: string]: unknown;
}

export default function Lighting() {
  const { data } = useStatus();
  const stage = useStage();
  const [config, setConfig] = useState<DaemonConfig | null>(null);

  const devices = data?.devices ?? [];

  // Display names come from the config; refetch when the configured set
  // changes (a bind/unbind landed). Failures fall back to kind strings
  // silently — the unreachable banner already tells that story.
  const configuredMacs = devices
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

  // Auto-select: first device when nothing (or a since-unbound device) is
  // selected; clear when the last device goes away.
  useEffect(() => {
    const macs = devices.map((d) => d.mac);
    if (stage.selectedMac !== null && macs.includes(stage.selectedMac)) return;
    if (stage.selectedMac === null && macs.length === 0) return;
    stageStore.selectDevice(macs[0] ?? null);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed on the mac set
  }, [configuredMacs, stage.selectedMac]);

  const cfgByMac = new Map((config?.devices ?? []).map((d) => [d.mac, d]));
  const selected = devices.find((d) => d.mac === stage.selectedMac) ?? null;

  // Stable identity across status polls; only kind/fan_count changes matter.
  const geometry = useMemo(
    () => (selected === null ? null : geometryForKind(selected.kind, selected.fan_count)),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- value-keyed
    [selected?.kind, selected?.fan_count],
  );

  return (
    <section className="section-content">
      <header className="section-header">
        <h1 className="section-title">Lighting</h1>
        <p className="section-subtitle">RGB effects, colours, and sync profiles</p>
      </header>

      {data === null ? (
        <div className="card-muted placeholder-card">
          <span>Waiting for daemon data</span>
          <span className="hint">polling status…</span>
        </div>
      ) : devices.length === 0 ? (
        <div className="card-muted placeholder-card">
          <span>No configured devices</span>
          <span className="hint">bind one from Devices to light the stage</span>
        </div>
      ) : (
        <>
          <div className="stage-picker" aria-label="stage device">
            {devices.map((d) => {
              const cfg = cfgByMac.get(d.mac);
              const label = cfg?.name?.trim() ? cfg.name : d.kind;
              const active = d.mac === stage.selectedMac;
              return (
                <button
                  key={d.mac}
                  type="button"
                  className={active ? 'stage-pill active' : 'stage-pill'}
                  aria-pressed={active}
                  title={d.mac}
                  onClick={() => stageStore.selectDevice(d.mac)}
                >
                  {label}
                </button>
              );
            })}
          </div>

          <div className="stage-layout">
            <div className="stage-main">
              {selected === null ? null : geometry === null ? (
                <div className="card-muted placeholder-card">
                  <span>no layout map for this device</span>
                  <span className="hint">{selected.kind} preview arrives post-v1</span>
                </div>
              ) : (
                <StageCanvas geometry={geometry} spec={stage.spec} />
              )}
            </div>

            {/* Task C4 seam: the effect rail (kind list, palette editor,
                speed/direction/brightness, Apply) mounts in this column. */}
            <div className="stage-rail" aria-hidden="true" />
          </div>
        </>
      )}
    </section>
  );
}
