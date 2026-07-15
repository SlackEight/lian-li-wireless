/**
 * Apply flow store — the per-mac state machine behind the effect rail's
 * Apply button:
 *
 *   idle → applying → settling → done (auto-idle after a beat)
 *              │          │
 *              │          └→ failed (12s without sync confirmation)
 *              └→ failed (daemon refusal, verbatim)
 *
 * The store issues `set_effect` itself (injected invoke) and concludes from
 * status snapshots fed through `noteStatus()`, watching the device's
 * `rgb_in_sync`. The daemon flips that false when the new frames upload and
 * true once post-settle readback confirms (~3s RF quiet window) — so a `true`
 * seen immediately after invoke may be STALE from the previous effect. We
 * only trust `true` after seeing the dip to false, or after
 * STALE_TRUE_POLLS polls without one (small uploads can confirm between
 * two of our 1s polls).
 *
 * Framework-free like bindFlow.ts: Vitest drives it with a mocked invoke,
 * fake timers, and synthetic StatusData snapshots.
 */
import type { StatusData, Timers } from './status.js';
import type { EffectSpec } from './stage.js';

/** Applying that hasn't confirmed after this long (from invoke-resolve) fails.
 * Live C5 measurement: a full SL-INF upload+settle+readback confirmed at ~9s,
 * so 12s left little margin — 20s covers slow polls without feeling stuck. */
export const APPLY_TIMEOUT_MS = 20_000;
/** Polls in `settling` after which a never-dipped `true` counts as confirmed. */
export const STALE_TRUE_POLLS = 2;
/** How long the "applied ✓" state lingers before returning to idle. */
export const DONE_LINGER_MS = 2500;

export type ApplyState =
  | { phase: 'applying' }
  | { phase: 'settling' }
  | { phase: 'done' }
  | { phase: 'failed'; message: string };

/** applying / settling — an apply that is still running. */
export function isActiveApply(state: ApplyState | undefined): boolean {
  return state !== undefined && (state.phase === 'applying' || state.phase === 'settling');
}

/** Per-mac apply states; macs absent from the record are idle. */
export type ApplyFlowSnapshot = Readonly<Record<string, ApplyState>>;

export type ApplyInvokeFn = (mac: string, spec: EffectSpec) => Promise<unknown>;

export interface ApplyFlow {
  getSnapshot(): ApplyFlowSnapshot;
  subscribe(onChange: () => void): () => void;
  /** Apply `spec` to `mac`; ignored while an apply for that mac is active. */
  start(mac: string, spec: EffectSpec): void;
  /** Feed every status poll here; settling applies conclude from it. */
  noteStatus(status: StatusData): void;
  /** Clear an apply back to idle (also cancels its timers). */
  dismiss(mac: string): void;
  /**
   * The spec the daemon last accepted for `mac` (set at invoke-resolve — the
   * daemon has committed it to config by then even if sync confirmation is
   * still pending), or null if nothing was applied this session. The rail
   * derives its dirty flag from this.
   */
  lastApplied(mac: string): EffectSpec | null;
}

interface Entry {
  state: ApplyState;
  /** Guards async invoke callbacks and timers from superseded applies. */
  seq: number;
  /** Timeout or done-linger timer, whichever phase owns it. */
  timer: unknown;
  /** Settling bookkeeping: saw rgb_in_sync dip to false / polls observed. */
  sawFalse: boolean;
  polls: number;
}

const defaultTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export function createApplyFlow(invoke: ApplyInvokeFn, opts: { timers?: Timers } = {}): ApplyFlow {
  const timers = opts.timers ?? defaultTimers;
  const entries = new Map<string, Entry>();
  const applied = new Map<string, EffectSpec>();
  let snapshot: ApplyFlowSnapshot = {};
  const listeners = new Set<() => void>();
  let nextSeq = 1;

  function publish() {
    const next: Record<string, ApplyState> = {};
    for (const [mac, entry] of entries) next[mac] = entry.state;
    snapshot = next;
    for (const cb of listeners) cb();
  }

  function clearTimer(entry: Entry) {
    if (entry.timer !== null) {
      timers.clear(entry.timer);
      entry.timer = null;
    }
  }

  /** The entry for `mac` iff it still belongs to the apply that spawned `seq`. */
  function live(mac: string, seq: number): Entry | null {
    const entry = entries.get(mac);
    return entry && entry.seq === seq ? entry : null;
  }

  function conclude(mac: string, entry: Entry, state: ApplyState) {
    clearTimer(entry);
    entry.state = state;
    if (state.phase === 'done') {
      const { seq } = entry;
      entry.timer = timers.set(() => {
        const cur = live(mac, seq);
        if (!cur || cur.state.phase !== 'done') return;
        entries.delete(mac);
        publish();
      }, DONE_LINGER_MS);
    }
    publish();
  }

  return {
    getSnapshot: () => snapshot,

    subscribe(onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },

    start(mac, spec) {
      if (isActiveApply(entries.get(mac)?.state)) return;
      const prior = entries.get(mac);
      if (prior) clearTimer(prior);
      const seq = nextSeq++;
      const entry: Entry = {
        state: { phase: 'applying' },
        seq,
        timer: null,
        sawFalse: false,
        polls: 0,
      };
      entries.set(mac, entry);
      publish();

      invoke(mac, spec).then(
        () => {
          const cur = live(mac, seq);
          if (!cur) return;
          applied.set(mac, spec);
          cur.state = { phase: 'settling' };
          cur.sawFalse = false;
          cur.polls = 0;
          cur.timer = timers.set(() => {
            const c = live(mac, seq);
            if (!c || c.state.phase !== 'settling') return;
            conclude(mac, c, {
              phase: 'failed',
              message: 'apply timed out — effect may not have committed; check Health',
            });
          }, APPLY_TIMEOUT_MS);
          publish();
        },
        (err: unknown) => {
          const cur = live(mac, seq);
          if (!cur) return;
          conclude(mac, cur, {
            phase: 'failed',
            message: typeof err === 'string' ? err : String(err),
          });
        },
      );
    },

    noteStatus(status) {
      for (const [mac, entry] of entries) {
        if (entry.state.phase !== 'settling') continue;
        const device = status.devices.find((d) => d.mac === mac);
        if (!device) continue;
        entry.polls += 1;
        if (device.rgb_in_sync === false) {
          entry.sawFalse = true;
        } else if (device.rgb_in_sync === true) {
          // Trust true after the dip, or once enough polls have passed that
          // a stale pre-apply true would have flipped false by now.
          if (entry.sawFalse || entry.polls > STALE_TRUE_POLLS) {
            conclude(mac, entry, { phase: 'done' });
          }
        }
      }
    },

    dismiss(mac) {
      const entry = entries.get(mac);
      if (!entry) return;
      clearTimer(entry);
      entry.seq = -1; // orphan any in-flight invoke callback
      entries.delete(mac);
      publish();
    },

    lastApplied(mac) {
      return applied.get(mac) ?? null;
    },
  };
}
