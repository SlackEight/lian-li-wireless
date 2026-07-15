/**
 * Status store — polls the daemon's `status` Tauri command and exposes the
 * latest snapshot as an external store (React consumes it through
 * `useSyncExternalStore`; see useStatus.ts).
 *
 * Framework-free on purpose: Vitest exercises the polling cadence, backoff,
 * and visibility handling with an injected invoke/visibility and fake timers —
 * no jsdom, no component mounting.
 */

/**
 * Stable marker prefixing every unreachable-socket error string from the Rust
 * layer (crates/llw-ui/src/ipc.rs `UNREACHABLE_PREFIX`). An error starting
 * with this means "daemon down"; any other error string is a daemon reply and
 * proves the daemon is reachable.
 */
export const UNREACHABLE_PREFIX = 'daemon unreachable';

/** Poll cadence while the daemon is reachable and the window is visible. */
export const POLL_MS = 1000;
/** Backoff cap while the daemon is unreachable (1s → 2s → 4s → 5s). */
export const BACKOFF_MAX_MS = 5000;

/* ── StatusData — mirror of llw-daemon's ipc::StatusData ── */

export interface LinkStatus {
  master_mac: string;
  channel: number;
}

export interface Telemetry {
  total_dropouts: number;
  total_tier1: number;
  total_tier2: number;
  failed_tier1_streak: number;
}

export interface DeviceStatus {
  mac: string;
  kind: string;
  channel: number;
  fan_count: number;
  /** Fixed length 4 — only the first `fan_count` entries are real fans. */
  rpm: number[];
  /** Fixed length 4, raw PWM bytes 0–255 per fan slot. */
  desired_pwm: number[];
  /** Fixed length 4, raw PWM bytes 0–255 per fan slot. */
  readback_pwm: number[];
  /** null = daemon has no expected effect (or no record yet) to compare. */
  rgb_in_sync: boolean | null;
  dropout_streak: number;
}

export interface AirDeviceStatus {
  mac: string;
  kind: string;
  bond: 'Ours' | 'Foreign' | 'Unbound';
  channel: number;
  fan_count: number;
  /** Fixed length 4 — slice to `fan_count`. */
  rpm: number[];
  last_seen_s: number;
}

export interface StatusData {
  daemon_version: string;
  /** null before channel/master acquisition. */
  link: LinkStatus | null;
  tx_wedged: boolean;
  reliability: Telemetry;
  devices: DeviceStatus[];
  air: AirDeviceStatus[];
  /** null or a pending bind/unbind op — typed properly in Task 5. */
  pending: unknown;
}

/**
 * The daemon reports pwm/rpm as fixed-length-4 arrays; trim to the slots that
 * hold real fans.
 */
export function sliceToFanCount<T>(values: readonly T[], fanCount: number): T[] {
  return values.slice(0, fanCount);
}

/* ── Store ── */

export interface StatusSnapshot {
  /** Last good status; kept (stale) while the daemon is unreachable. */
  data: StatusData | null;
  /** false only while errors carry UNREACHABLE_PREFIX (or invoke itself blew up). */
  daemonReachable: boolean;
  /** Most recent error, verbatim; null after a successful poll. */
  lastError: string | null;
}

export type InvokeFn = (cmd: string) => Promise<unknown>;

export interface Timers {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

export interface VisibilitySource {
  visible(): boolean;
  /** Subscribe to visibility flips; returns an unsubscribe. */
  onChange(cb: () => void): () => void;
}

export interface StatusStoreOptions {
  timers?: Timers;
  visibility?: VisibilitySource;
}

export interface StatusStore {
  getSnapshot(): StatusSnapshot;
  subscribe(onChange: () => void): () => void;
  start(): void;
  stop(): void;
}

const defaultTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/** Default visibility: the document, or "always visible" in non-DOM hosts. */
function documentVisibility(): VisibilitySource {
  if (typeof document === 'undefined') {
    return { visible: () => true, onChange: () => () => {} };
  }
  return {
    visible: () => document.visibilityState === 'visible',
    onChange: (cb) => {
      document.addEventListener('visibilitychange', cb);
      return () => document.removeEventListener('visibilitychange', cb);
    },
  };
}

export function createStatusStore(invoke: InvokeFn, opts: StatusStoreOptions = {}): StatusStore {
  const timers = opts.timers ?? defaultTimers;
  const visibility = opts.visibility ?? documentVisibility();

  let snapshot: StatusSnapshot = { data: null, daemonReachable: true, lastError: null };
  const listeners = new Set<() => void>();

  let running = false;
  let timer: unknown = null;
  let backoffMs = POLL_MS;
  // Bumped whenever the poll chain (re)starts or pauses; a poll still in
  // flight from an older generation drops its result instead of racing the
  // new chain (or updating state after stop/hide).
  let generation = 0;
  let unsubVisibility: (() => void) | null = null;

  function setSnapshot(next: StatusSnapshot) {
    snapshot = next;
    for (const cb of listeners) cb();
  }

  /** Failure path: keep last good data; skip the notify if nothing changed. */
  function patchSnapshot(reachable: boolean, msg: string) {
    if (snapshot.daemonReachable === reachable && snapshot.lastError === msg) return;
    setSnapshot({ ...snapshot, daemonReachable: reachable, lastError: msg });
  }

  function cancelTimer() {
    if (timer !== null) {
      timers.clear(timer);
      timer = null;
    }
  }

  function schedule(ms: number) {
    if (!running || !visibility.visible()) return;
    timer = timers.set(() => {
      timer = null;
      void poll();
    }, ms);
  }

  async function poll(): Promise<void> {
    const gen = generation;
    try {
      const data = await invoke('status');
      if (gen !== generation) return;
      backoffMs = POLL_MS;
      setSnapshot({ data: data as StatusData, daemonReachable: true, lastError: null });
      schedule(POLL_MS);
    } catch (err) {
      if (gen !== generation) return;
      const msg = typeof err === 'string' ? err : String(err);
      // The Rust layer rejects with plain strings; only UNREACHABLE_PREFIX
      // means "daemon down". A non-string rejection means invoke itself
      // failed (e.g. plain-browser dev without a Tauri runtime) — the daemon
      // is just as unreachable from where we stand.
      const unreachable = typeof err !== 'string' || msg.startsWith(UNREACHABLE_PREFIX);
      if (unreachable) {
        const delay = backoffMs;
        backoffMs = Math.min(backoffMs * 2, BACKOFF_MAX_MS);
        patchSnapshot(false, msg);
        schedule(delay);
      } else {
        // The daemon replied (with a refusal) — reachable, normal cadence.
        backoffMs = POLL_MS;
        patchSnapshot(true, msg);
        schedule(POLL_MS);
      }
    }
  }

  function restart() {
    generation += 1;
    cancelTimer();
    void poll();
  }

  function onVisibilityChange() {
    if (!running) return;
    if (visibility.visible()) {
      restart(); // resume with an immediate poll
    } else {
      generation += 1; // drop any in-flight result
      cancelTimer();
    }
  }

  return {
    getSnapshot: () => snapshot,
    subscribe(onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },
    start() {
      if (running) return;
      running = true;
      backoffMs = POLL_MS;
      unsubVisibility = visibility.onChange(onVisibilityChange);
      if (visibility.visible()) restart();
    },
    stop() {
      if (!running) return;
      running = false;
      generation += 1;
      cancelTimer();
      unsubVisibility?.();
      unsubVisibility = null;
    },
  };
}
