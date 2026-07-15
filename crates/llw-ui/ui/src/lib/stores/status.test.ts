import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  createStatusStore,
  sliceToFanCount,
  UNREACHABLE_PREFIX,
  type StatusData,
  type VisibilitySource,
} from './status.js';

// Real captured daemon payload (ground truth from the running daemon).
const PAYLOAD: StatusData = {
  air: [
    {
      bond: 'Ours',
      channel: 8,
      fan_count: 3,
      kind: 'UNI FAN SL-INF Wireless',
      last_seen_s: 0,
      mac: '02:8b:51:62:32:e1',
      rpm: [2187, 2187, 2206, 0],
    },
  ],
  daemon_version: '0.1.0',
  devices: [
    {
      channel: 8,
      desired_pwm: [86, 86, 86, 0],
      dropout_streak: 41,
      fan_count: 3,
      kind: 'UNI FAN SL-INF Wireless',
      mac: '02:8b:51:62:32:e1',
      readback_pwm: [0, 0, 0, 0],
      rgb_in_sync: true,
      rpm: [2187, 2187, 2206, 0],
    },
  ],
  link: { channel: 8, master_mac: 'e5:ba:f0:72:ab:3c' },
  pending: null,
  reliability: { failed_tier1_streak: 0, total_dropouts: 220, total_tier1: 5, total_tier2: 0 },
  tx_wedged: false,
};

const UNREACHABLE = `${UNREACHABLE_PREFIX}: connecting /run/user/1000/llw-daemon.sock: refused`;

/** Injectable visibility fake — the store is tested without any DOM. */
function fakeVisibility(initiallyVisible = true) {
  let isVisible = initiallyVisible;
  const subs = new Set<() => void>();
  const source: VisibilitySource = {
    visible: () => isVisible,
    onChange: (cb) => {
      subs.add(cb);
      return () => subs.delete(cb);
    },
  };
  return {
    source,
    set(visible: boolean) {
      isVisible = visible;
      for (const cb of subs) cb();
    },
  };
}

// Flush the immediate (non-timer) poll's microtasks.
const flush = () => vi.advanceTimersByTimeAsync(0);

describe('status store', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('polls immediately on start and then every 1s while visible', async () => {
    const invoke = vi.fn().mockResolvedValue(PAYLOAD);
    const store = createStatusStore(invoke);

    store.start();
    await flush();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('status');
    expect(store.getSnapshot()).toEqual({ data: PAYLOAD, daemonReachable: true, lastError: null });

    await vi.advanceTimersByTimeAsync(999);
    expect(invoke).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(invoke).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(2000);
    expect(invoke).toHaveBeenCalledTimes(4);
    store.stop();
  });

  it('stops polling after stop()', async () => {
    const invoke = vi.fn().mockResolvedValue(PAYLOAD);
    const store = createStatusStore(invoke);

    store.start();
    await flush();
    store.stop();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it('notifies subscribers on updates; unsubscribe detaches', async () => {
    const invoke = vi.fn().mockResolvedValue(PAYLOAD);
    const store = createStatusStore(invoke);
    const onChange = vi.fn();
    const unsubscribe = store.subscribe(onChange);

    store.start();
    await flush();
    expect(onChange).toHaveBeenCalledTimes(1);

    unsubscribe();
    await vi.advanceTimersByTimeAsync(1000);
    expect(onChange).toHaveBeenCalledTimes(1);
    store.stop();
  });

  it('pauses polling while hidden and resumes with an immediate poll', async () => {
    const invoke = vi.fn().mockResolvedValue(PAYLOAD);
    const vis = fakeVisibility(true);
    const store = createStatusStore(invoke, { visibility: vis.source });

    store.start();
    await flush();
    expect(invoke).toHaveBeenCalledTimes(1);

    vis.set(false);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(invoke).toHaveBeenCalledTimes(1); // paused

    vis.set(true);
    await flush();
    expect(invoke).toHaveBeenCalledTimes(2); // immediate poll on resume
    await vi.advanceTimersByTimeAsync(1000);
    expect(invoke).toHaveBeenCalledTimes(3); // cadence resumed
    store.stop();
  });

  it('does not poll when started hidden; first poll arrives on visible', async () => {
    const invoke = vi.fn().mockResolvedValue(PAYLOAD);
    const vis = fakeVisibility(false);
    const store = createStatusStore(invoke, { visibility: vis.source });

    store.start();
    await vi.advanceTimersByTimeAsync(5000);
    expect(invoke).toHaveBeenCalledTimes(0);

    vis.set(true);
    await flush();
    expect(invoke).toHaveBeenCalledTimes(1);
    store.stop();
  });

  it('an unreachable-prefixed error flips daemonReachable and keeps last good data', async () => {
    const invoke = vi.fn().mockResolvedValueOnce(PAYLOAD).mockRejectedValue(UNREACHABLE);
    const store = createStatusStore(invoke);

    store.start();
    await flush();
    await vi.advanceTimersByTimeAsync(1000);

    const snap = store.getSnapshot();
    expect(snap.daemonReachable).toBe(false);
    expect(snap.lastError).toBe(UNREACHABLE);
    expect(snap.data).toEqual(PAYLOAD); // stale data stays rendered
    store.stop();
  });

  it('backs off 1s → 2s → 4s → 5s (cap) while unreachable', async () => {
    const invoke = vi.fn().mockRejectedValue(UNREACHABLE);
    const store = createStatusStore(invoke);

    store.start();
    await flush();
    expect(invoke).toHaveBeenCalledTimes(1);

    // 1s gap
    await vi.advanceTimersByTimeAsync(999);
    expect(invoke).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(invoke).toHaveBeenCalledTimes(2);
    // 2s gap
    await vi.advanceTimersByTimeAsync(1999);
    expect(invoke).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(invoke).toHaveBeenCalledTimes(3);
    // 4s gap
    await vi.advanceTimersByTimeAsync(3999);
    expect(invoke).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(1);
    expect(invoke).toHaveBeenCalledTimes(4);
    // 5s cap
    await vi.advanceTimersByTimeAsync(4999);
    expect(invoke).toHaveBeenCalledTimes(4);
    await vi.advanceTimersByTimeAsync(1);
    expect(invoke).toHaveBeenCalledTimes(5);
    // stays at 5s
    await vi.advanceTimersByTimeAsync(5000);
    expect(invoke).toHaveBeenCalledTimes(6);
    store.stop();
  });

  it('first success resets to the 1s cadence and clears the error', async () => {
    const invoke = vi
      .fn()
      .mockRejectedValueOnce(UNREACHABLE)
      .mockRejectedValueOnce(UNREACHABLE)
      .mockResolvedValue(PAYLOAD);
    const store = createStatusStore(invoke);

    store.start();
    await flush(); // fail #1 → next in 1s
    await vi.advanceTimersByTimeAsync(1000); // fail #2 → next in 2s
    await vi.advanceTimersByTimeAsync(2000); // success
    expect(invoke).toHaveBeenCalledTimes(3);
    expect(store.getSnapshot()).toEqual({ data: PAYLOAD, daemonReachable: true, lastError: null });

    await vi.advanceTimersByTimeAsync(1000); // back to 1s cadence
    expect(invoke).toHaveBeenCalledTimes(4);
    store.stop();
  });

  it('a non-prefixed error string is a request failure: reachable, data kept, error verbatim', async () => {
    const refusal = 'no such device: aa:bb:cc:dd:ee:ff';
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(PAYLOAD)
      .mockRejectedValueOnce(refusal)
      .mockResolvedValue(PAYLOAD);
    const store = createStatusStore(invoke);

    store.start();
    await flush();
    await vi.advanceTimersByTimeAsync(1000); // the failure

    const snap = store.getSnapshot();
    expect(snap.daemonReachable).toBe(true); // NOT unreachability
    expect(snap.lastError).toBe(refusal); // verbatim
    expect(snap.data).toEqual(PAYLOAD);

    await vi.advanceTimersByTimeAsync(1000); // cadence unchanged (no backoff)
    expect(invoke).toHaveBeenCalledTimes(3);
    expect(store.getSnapshot().lastError).toBeNull();
    store.stop();
  });

  it('a non-string rejection (no Tauri runtime) counts as unreachable', async () => {
    const invoke = vi
      .fn()
      .mockRejectedValue(new TypeError("window.__TAURI_INTERNALS__ is undefined"));
    const store = createStatusStore(invoke);

    store.start();
    await flush();

    const snap = store.getSnapshot();
    expect(snap.daemonReachable).toBe(false);
    expect(snap.lastError).toContain('__TAURI_INTERNALS__');
    store.stop();
  });

  it('getSnapshot is referentially stable between changes (and across identical failures)', async () => {
    const invoke = vi.fn().mockRejectedValue(UNREACHABLE);
    const store = createStatusStore(invoke);

    expect(store.getSnapshot()).toBe(store.getSnapshot());

    store.start();
    await flush(); // fail #1
    const afterFirstFailure = store.getSnapshot();
    await vi.advanceTimersByTimeAsync(1000); // fail #2, identical outcome
    expect(store.getSnapshot()).toBe(afterFirstFailure); // no churn
    store.stop();
  });

  it('sliceToFanCount trims the fixed-length-4 arrays to real fans', () => {
    expect(sliceToFanCount([2187, 2187, 2206, 0], 3)).toEqual([2187, 2187, 2206]);
    expect(sliceToFanCount([86, 86, 86, 0], 4)).toEqual([86, 86, 86, 0]);
    expect(sliceToFanCount([0, 0, 0, 0], 0)).toEqual([]);
  });
});
