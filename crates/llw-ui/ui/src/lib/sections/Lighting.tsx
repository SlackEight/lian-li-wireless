/**
 * Lighting stage (M4c): device picker pills + the live WASM-rendered
 * StageCanvas hero, the effect rail (C4), and preset chips. The working
 * effect spec lives in the stage store singleton (stores/stage.ts) so the
 * rail edits the same spec the canvas previews.
 */
import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useStatus } from '../stores/useStatus.js';
import { useStage } from '../stores/useStage.js';
import {
  stageStore,
  geometryForKind,
  specFromWire,
  clampSpec,
  type EffectSpec,
  type EffectKind,
} from '../stores/stage.js';
import { toastStore } from '../stores/useToasts.js';
import StageCanvas from '../components/StageCanvas.js';
import EffectRail from '../components/EffectRail.js';
import ConfirmDialog from '../components/ConfirmDialog.js';
import NamePromptDialog from '../components/NamePromptDialog.js';

/* ── Config mirror — names + presets; presets are mutated via the standard
      get_config → mutate → set_config round-trip (unknown fields survive
      through the index signatures) ── */

interface ConfigDevice {
  mac: string;
  name: string | null;
  [key: string]: unknown;
}

interface ConfigPreset {
  name: string;
  /** Wire shape — may rely on serde defaults; load through specFromWire. */
  effect: Partial<EffectSpec> & { kind: EffectKind };
  [key: string]: unknown;
}

interface DaemonConfig {
  devices: ConfigDevice[];
  presets?: ConfigPreset[];
  [key: string]: unknown;
}

export default function Lighting() {
  const { data } = useStatus();
  const stage = useStage();
  const [config, setConfig] = useState<DaemonConfig | null>(null);
  const [saveOpen, setSaveOpen] = useState(false);
  const [deletePreset, setDeletePreset] = useState<string | null>(null);
  const [configBump, setConfigBump] = useState(0);

  const devices = data?.devices ?? [];

  // Display names + presets come from the config; refetch when the
  // configured set changes (a bind/unbind landed) or after our own preset
  // writes. Failures fall back silently — the unreachable banner already
  // tells that story.
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
  }, [configuredMacs, configBump]);

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
  const presets = config?.presets ?? [];

  // Stable identity across status polls; only kind/fan_count changes matter.
  const geometry = useMemo(
    () => (selected === null ? null : geometryForKind(selected.kind, selected.fan_count)),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- value-keyed
    [selected?.kind, selected?.fan_count],
  );

  /** get_config → mutate presets → set_config; toast failures verbatim. */
  function writePresets(mutate: (presets: ConfigPreset[]) => ConfigPreset[]) {
    invoke('get_config')
      .then((raw) => {
        const cfg = raw as DaemonConfig;
        const next = { ...cfg, presets: mutate(cfg.presets ?? []) };
        return invoke('set_config', { json: next });
      })
      .then(() => setConfigBump((n) => n + 1))
      .catch((err: unknown) => {
        toastStore.push('error', typeof err === 'string' ? err : String(err));
      });
  }

  function onSavePreset(name: string) {
    setSaveOpen(false);
    const trimmed = name.trim();
    if (presets.some((p) => p.name === trimmed)) {
      toastStore.push('warn', `a preset named "${trimmed}" already exists`);
      return;
    }
    writePresets((ps) => [...ps, { name: trimmed, effect: clampSpec(stage.spec) }]);
  }

  function onDeletePreset(name: string) {
    setDeletePreset(null);
    writePresets((ps) => ps.filter((p) => p.name !== name));
  }

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

            <div className="stage-rail">
              <EffectRail mac={stage.selectedMac} />
            </div>
          </div>

          <div className="preset-row" aria-label="presets">
            {presets.map((p) => (
              <span key={p.name} className="preset-chip-wrap">
                <button
                  type="button"
                  className="preset-chip"
                  title="load preset (does not apply)"
                  onClick={() => stageStore.setSpec(specFromWire(p.effect))}
                >
                  {p.name}
                </button>
                <button
                  type="button"
                  className="preset-delete"
                  title={`delete preset ${p.name}`}
                  aria-label={`delete preset ${p.name}`}
                  onClick={() => setDeletePreset(p.name)}
                >
                  ✕
                </button>
              </span>
            ))}
            <button type="button" className="preset-chip preset-save" onClick={() => setSaveOpen(true)}>
              + save preset
            </button>
          </div>
        </>
      )}

      {saveOpen && (
        <NamePromptDialog
          title="Save preset"
          confirmLabel="Save"
          placeholder="preset name"
          onConfirm={onSavePreset}
          onCancel={() => setSaveOpen(false)}
        />
      )}

      {deletePreset !== null && (
        <ConfirmDialog
          title="Delete preset"
          body={`Delete the preset "${deletePreset}"? Devices keep whatever effect they are running.`}
          confirmLabel="Delete"
          onConfirm={() => onDeletePreset(deletePreset)}
          onCancel={() => setDeletePreset(null)}
        />
      )}
    </section>
  );
}
