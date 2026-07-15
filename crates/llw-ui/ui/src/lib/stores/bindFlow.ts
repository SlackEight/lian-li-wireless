/**
 * Bind/unbind flow store — the per-mac operation state machine behind the
 * Devices screen:
 *
 *   idle → requesting → converging → done
 *              │             │
 *              │             └→ failed (pending "failed", or 15s timeout)
 *              ├→ settling-retry(n) → requesting …  (≤3 attempts, 2s gaps)
 *              └→ failed (daemon refusal, verbatim)
 *
 * The store issues the `bind`/`unbind` Tauri commands itself (injected
 * invoke — the Rust layer does NOT retry) and concludes convergence from
 * status snapshots fed in through `noteStatus()`, watching the same signals
 * the CLI's poll_bind_convergence does: the `pending` op plus the mac's
 * membership in `devices`/air-Ours.
 *
 * Framework-free like status.ts: Vitest drives it with a mocked invoke,
 * fake timers, and synthetic StatusData snapshots.
 */
import type { StatusData, Timers } from './status.js';

/**
 * Substring of the daemon's transient refusal while the radio settles after
 * recent bind/unbind/RGB traffic (mirrors llw-cli's SETTLING_ERROR — the
 * exact string it retries on).
 */
export const SETTLING_MARKER = 'radio settling';
/** Gap between settling retries (mirrors the CLI's 2s). */
export const SETTLING_RETRY_GAP_MS = 2000;
/** Total request attempts when refused as settling (mirrors the CLI's 3). */
export const SETTLING_MAX_ATTEMPTS = 3;
/**
 * A converging op with no verdict after this long is declared failed.
 * Convergence is normally ≤5s (daemon polls GetDev); the CLI gives up at 12s.
 */
export const CONVERGENCE_TIMEOUT_MS = 15_000;

export type OpKind = 'bind' | 'unbind';

export type OpState =
  | { op: OpKind; phase: 'requesting' }
  | { op: OpKind; phase: 'settling-retry'; attempt: number }
  | { op: OpKind; phase: 'converging' }
  | { op: OpKind; phase: 'done' }
  | { op: OpKind; phase: 'failed'; message: string };

/** requesting / settling-retry / converging — an op that is still running. */
export function isActiveOp(state: OpState | undefined): boolean {
  return state !== undefined && state.phase !== 'done' && state.phase !== 'failed';
}

/** Per-mac op states; macs absent from the record are idle. */
export type BindFlowSnapshot = Readonly<Record<string, OpState>>;

export type OpInvokeFn = (cmd: OpKind, args: { mac: string }) => Promise<unknown>;

export interface BindFlow {
  getSnapshot(): BindFlowSnapshot;
  subscribe(onChange: () => void): () => void;
  /** Kick off a bind/unbind for `mac`; ignored while one is already active. */
  start(op: OpKind, mac: string): void;
  /** Feed every status poll here; converging ops conclude from it. */
  noteStatus(status: StatusData): void;
  /** Clear an op back to idle (also cancels its timers). */
  dismiss(mac: string): void;
}

interface Entry {
  state: OpState;
  /** Guards async invoke callbacks and timers from superseded ops. */
  seq: number;
  /** Retry-gap or convergence-deadline timer, whichever phase owns it. */
  timer: unknown;
}

const defaultTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export function createBindFlow(invoke: OpInvokeFn, opts: { timers?: Timers } = {}): BindFlow {
  const timers = opts.timers ?? defaultTimers;
  const entries = new Map<string, Entry>();
  let snapshot: BindFlowSnapshot = {};
  const listeners = new Set<() => void>();
  let nextSeq = 1;

  function publish() {
    const next: Record<string, OpState> = {};
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

  /** The entry for `mac` iff it still belongs to the op that spawned `seq`. */
  function live(mac: string, seq: number): Entry | null {
    const entry = entries.get(mac);
    return entry && entry.seq === seq ? entry : null;
  }

  function attempt(mac: string, entry: Entry, n: number) {
    const { seq } = entry;
    const op = entry.state.op;
    invoke(op, { mac }).then(
      () => {
        const cur = live(mac, seq);
        if (!cur) return;
        cur.state = { op, phase: 'converging' };
        // Deadline: if noteStatus never concludes the op, fail it loudly
        // rather than spinning forever.
        cur.timer = timers.set(() => {
          const c = live(mac, seq);
          if (!c || c.state.phase !== 'converging') return;
          c.timer = null;
          c.state = {
            op,
            phase: 'failed',
            message: `${op} timed out — no convergence after ${CONVERGENCE_TIMEOUT_MS / 1000}s; check Health`,
          };
          publish();
        }, CONVERGENCE_TIMEOUT_MS);
        publish();
      },
      (err: unknown) => {
        const cur = live(mac, seq);
        if (!cur) return;
        const message = typeof err === 'string' ? err : String(err);
        if (message.includes(SETTLING_MARKER) && n < SETTLING_MAX_ATTEMPTS) {
          // Transient: mirror the CLI's UX — retry after 2s, up to 3 attempts.
          cur.state = { op, phase: 'settling-retry', attempt: n };
          cur.timer = timers.set(() => {
            const c = live(mac, seq);
            if (!c) return;
            c.timer = null;
            c.state = { op, phase: 'requesting' };
            publish();
            attempt(mac, c, n + 1);
          }, SETTLING_RETRY_GAP_MS);
        } else {
          // Refusal (or settling attempts exhausted): verbatim daemon string.
          clearTimer(cur);
          cur.state = { op, phase: 'failed', message };
        }
        publish();
      },
    );
  }

  return {
    getSnapshot: () => snapshot,

    subscribe(onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },

    start(op, mac) {
      const key = mac.toLowerCase();
      const existing = entries.get(key);
      if (existing && isActiveOp(existing.state)) return;
      if (existing) clearTimer(existing);
      const entry: Entry = { state: { op, phase: 'requesting' }, seq: nextSeq++, timer: null };
      entries.set(key, entry);
      publish();
      attempt(key, entry, 1);
    },

    noteStatus(status) {
      let changed = false;
      for (const [mac, entry] of entries) {
        const st = entry.state;
        // "Membership": bound + configured from where the UI stands — the mac
        // is in `devices`, or its air record classifies as Ours.
        const member =
          status.devices.some((d) => d.mac === mac) ||
          status.air.some((a) => a.mac === mac && a.bond === 'Ours');

        if (st.phase === 'converging') {
          const p = status.pending;
          if (p !== null && p.mac === mac && p.op === st.op && p.state === 'failed') {
            clearTimer(entry);
            entry.state = {
              op: st.op,
              phase: 'failed',
              message: `${st.op} failed — device did not converge`,
            };
            changed = true;
            continue;
          }
          // Same success gate as the CLI: membership flipped AND no live
          // pending op for this mac. Anything else = still converging.
          const stillPending = p !== null && p.mac === mac && p.state !== 'failed';
          const settled = st.op === 'bind' ? member && !stillPending : !member && !stillPending;
          if (settled) {
            clearTimer(entry);
            entry.state = { op: st.op, phase: 'done' };
            changed = true;
          }
        } else if (st.phase === 'done') {
          // Hygiene: a stale ✓ whose ground truth flipped back (e.g. the CLI
          // re-bound/unbound the device behind our back) returns to idle.
          if ((st.op === 'bind' && !member) || (st.op === 'unbind' && member)) {
            entries.delete(mac);
            changed = true;
          }
        }
      }
      if (changed) publish();
    },

    dismiss(mac) {
      const key = mac.toLowerCase();
      const entry = entries.get(key);
      if (!entry) return;
      clearTimer(entry);
      entries.delete(key); // in-flight callbacks drop via the seq guard
      publish();
    },
  };
}
