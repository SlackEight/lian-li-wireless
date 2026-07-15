import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  createBindFlow,
  isActiveOp,
  CONVERGENCE_TIMEOUT_MS,
  SETTLING_MAX_ATTEMPTS,
  SETTLING_RETRY_GAP_MS,
} from './bindFlow.js';
import type { AirDeviceStatus, DeviceStatus, PendingOp, StatusData } from './status.js';

const MAC = '02:8b:51:62:32:e1';

const ACCEPTED = { state: 'started' };
const REFUSAL = 'refusing to bind: device is bound to another master';
const SETTLING = 'bind refused: radio settling after recent RF activity';

function airEntry(mac: string, bond: AirDeviceStatus['bond']): AirDeviceStatus {
  return {
    mac,
    kind: 'UNI FAN SL-INF Wireless',
    bond,
    channel: 8,
    fan_count: 3,
    rpm: [2187, 2187, 2206, 0],
    last_seen_s: 0,
  };
}

function deviceEntry(mac: string): DeviceStatus {
  return {
    mac,
    kind: 'UNI FAN SL-INF Wireless',
    channel: 8,
    fan_count: 3,
    rpm: [2187, 2187, 2206, 0],
    desired_pwm: [86, 86, 86, 0],
    readback_pwm: [86, 86, 86, 0],
    rgb_in_sync: true,
    dropout_streak: 0,
  };
}

/** Synthetic status snapshot: name the configured macs, air rows, pending. */
function statusWith(
  opts: { devices?: string[]; air?: AirDeviceStatus[]; pending?: PendingOp | null } = {},
): StatusData {
  return {
    daemon_version: '0.1.0',
    link: { master_mac: 'e5:ba:f0:72:ab:3c', channel: 8 },
    tx_wedged: false,
    reliability: { total_dropouts: 0, total_tier1: 0, total_tier2: 0, failed_tier1_streak: 0 },
    devices: (opts.devices ?? []).map(deviceEntry),
    air: opts.air ?? [],
    pending: opts.pending ?? null,
  };
}

// Flush the immediate (non-timer) invoke settlement microtasks.
const flush = () => vi.advanceTimersByTimeAsync(0);

describe('bind flow', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('bind: requesting → converging → done when the mac lands in devices', async () => {
    const invoke = vi.fn().mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'requesting' });
    expect(invoke).toHaveBeenCalledExactlyOnceWith('bind', { mac: MAC });

    await flush();
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'converging' });

    // Daemon still tracking the pending op → not done yet.
    flow.noteStatus(statusWith({ pending: { op: 'bind', mac: MAC, state: 'converging' } }));
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'converging' });

    // Mac in devices, pending cleared → done; the deadline timer must be dead.
    flow.noteStatus(statusWith({ devices: [MAC] }));
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'done' });
    await vi.advanceTimersByTimeAsync(CONVERGENCE_TIMEOUT_MS + 1000);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'done' });
  });

  it('bind: an air record flipping to Ours also counts as membership', async () => {
    const invoke = vi.fn().mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();
    flow.noteStatus(statusWith({ air: [airEntry(MAC, 'Ours')] }));
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'done' });
  });

  it('a daemon refusal fails with the verbatim string', async () => {
    const invoke = vi.fn().mockRejectedValue(REFUSAL);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'failed', message: REFUSAL });
    expect(invoke).toHaveBeenCalledTimes(1); // no retry for non-settling refusals
  });

  it('settling refusals retry on 2s gaps, then surface the string verbatim', async () => {
    const invoke = vi.fn().mockRejectedValue(SETTLING);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'settling-retry', attempt: 1 });

    // Retry #2 fires exactly at the 2s mark.
    await vi.advanceTimersByTimeAsync(SETTLING_RETRY_GAP_MS - 1);
    expect(invoke).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'settling-retry', attempt: 2 });

    // Retry #3 (the last allowed attempt) fails for good, message verbatim.
    await vi.advanceTimersByTimeAsync(SETTLING_RETRY_GAP_MS);
    expect(invoke).toHaveBeenCalledTimes(SETTLING_MAX_ATTEMPTS);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'failed', message: SETTLING });

    // No further attempts after exhaustion.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(invoke).toHaveBeenCalledTimes(SETTLING_MAX_ATTEMPTS);
  });

  it('a settling refusal that clears mid-retry converges normally', async () => {
    const invoke = vi.fn().mockRejectedValueOnce(SETTLING).mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'settling-retry', attempt: 1 });

    await vi.advanceTimersByTimeAsync(SETTLING_RETRY_GAP_MS);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'converging' });

    flow.noteStatus(statusWith({ devices: [MAC] }));
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'done' });
  });

  it('pending state "failed" fails the op with a clear message', async () => {
    const invoke = vi.fn().mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();
    flow.noteStatus(statusWith({ pending: { op: 'bind', mac: MAC, state: 'failed' } }));
    expect(flow.getSnapshot()[MAC]).toEqual({
      op: 'bind',
      phase: 'failed',
      message: 'bind failed — device did not converge',
    });
  });

  it('a converging op with no verdict times out as failed after 15s', async () => {
    const invoke = vi.fn().mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();

    // Inconclusive polls (no membership, no pending) keep it converging.
    flow.noteStatus(statusWith());
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'converging' });

    await vi.advanceTimersByTimeAsync(CONVERGENCE_TIMEOUT_MS - 1);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'converging' });
    await vi.advanceTimersByTimeAsync(1);
    const state = flow.getSnapshot()[MAC];
    expect(state.phase).toBe('failed');
    expect(state.phase === 'failed' && state.message).toContain('15s');
  });

  it('unbind: waits while the mac is still configured, done once it leaves', async () => {
    const invoke = vi.fn().mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('unbind', MAC);
    expect(invoke).toHaveBeenCalledExactlyOnceWith('unbind', { mac: MAC });
    await flush();
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'unbind', phase: 'converging' });

    // Still in devices + pending live → keep converging.
    flow.noteStatus(
      statusWith({ devices: [MAC], pending: { op: 'unbind', mac: MAC, state: 'converging' } }),
    );
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'unbind', phase: 'converging' });

    // Gone from devices but pending not yet cleared → keep converging.
    flow.noteStatus(statusWith({ pending: { op: 'unbind', mac: MAC, state: 'converging' } }));
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'unbind', phase: 'converging' });

    // Gone from devices and pending cleared → done.
    flow.noteStatus(statusWith({ air: [airEntry(MAC, 'Unbound')] }));
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'unbind', phase: 'done' });
  });

  it('start is ignored while an op for the same mac is active', async () => {
    const invoke = vi.fn().mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    flow.start('bind', MAC); // still requesting
    await flush();
    flow.start('unbind', MAC); // converging — also ignored
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'converging' });
  });

  it('dismiss clears a settled op back to idle and allows a restart', async () => {
    const invoke = vi.fn().mockRejectedValueOnce(REFUSAL).mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();
    expect(flow.getSnapshot()[MAC]?.phase).toBe('failed');

    flow.dismiss(MAC);
    expect(flow.getSnapshot()).toEqual({});

    flow.start('bind', MAC);
    await flush();
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'converging' });
  });

  it('a stale done resets to idle when ground truth flips back', async () => {
    const invoke = vi.fn().mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();
    flow.noteStatus(statusWith({ devices: [MAC] }));
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'done' });

    // Someone unbound it behind our back (e.g. via the CLI) — the ✓ clears.
    flow.noteStatus(statusWith({ air: [airEntry(MAC, 'Unbound')] }));
    expect(flow.getSnapshot()[MAC]).toBeUndefined();
  });

  it('a non-string rejection fails with its string form', async () => {
    const invoke = vi.fn().mockRejectedValue(new TypeError('window.__TAURI_INTERNALS__ is undefined'));
    const flow = createBindFlow(invoke);

    flow.start('bind', MAC);
    await flush();
    const state = flow.getSnapshot()[MAC];
    expect(state.phase).toBe('failed');
    expect(state.phase === 'failed' && state.message).toContain('__TAURI_INTERNALS__');
  });

  it('notifies subscribers on transitions; unsubscribe detaches', async () => {
    const invoke = vi.fn().mockResolvedValue(ACCEPTED);
    const flow = createBindFlow(invoke);
    const onChange = vi.fn();
    const unsubscribe = flow.subscribe(onChange);

    flow.start('bind', MAC); // requesting
    await flush(); // converging
    expect(onChange).toHaveBeenCalledTimes(2);

    unsubscribe();
    flow.noteStatus(statusWith({ devices: [MAC] })); // done
    expect(onChange).toHaveBeenCalledTimes(2);
    expect(flow.getSnapshot()[MAC]).toEqual({ op: 'bind', phase: 'done' });
  });

  it('isActiveOp: active phases only', () => {
    expect(isActiveOp(undefined)).toBe(false);
    expect(isActiveOp({ op: 'bind', phase: 'requesting' })).toBe(true);
    expect(isActiveOp({ op: 'bind', phase: 'settling-retry', attempt: 1 })).toBe(true);
    expect(isActiveOp({ op: 'unbind', phase: 'converging' })).toBe(true);
    expect(isActiveOp({ op: 'bind', phase: 'done' })).toBe(false);
    expect(isActiveOp({ op: 'unbind', phase: 'failed', message: 'x' })).toBe(false);
  });
});
