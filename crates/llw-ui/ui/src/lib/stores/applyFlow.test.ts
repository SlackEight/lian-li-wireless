import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  createApplyFlow,
  isActiveApply,
  APPLY_TIMEOUT_MS,
  STALE_TRUE_POLLS,
  DONE_LINGER_MS,
  type ApplyFlow,
} from './applyFlow.js';
import { defaultSpec, type EffectSpec } from './stage.js';
import type { StatusData, Timers } from './status.js';

const MAC = '02:8b:51:62:32:e1';

function statusWithSync(sync: boolean | null, mac = MAC): StatusData {
  return {
    daemon_version: '0.1.0',
    link: { master_mac: 'e5:ba:f0:72:ab:3c', channel: 8 },
    tx_wedged: false,
    reliability: { total_dropouts: 0, total_tier1: 0, total_tier2: 0, failed_tier1_streak: 0 },
    devices: [
      {
        mac,
        kind: 'UNI FAN SL-INF Wireless',
        channel: 8,
        fan_count: 3,
        rpm: [800, 800, 800, 0],
        desired_pwm: [86, 86, 86, 0],
        readback_pwm: [86, 86, 86, 0],
        rgb_in_sync: sync,
        dropout_streak: 0,
      },
    ],
    air: [],
    pending: null,
  };
}

const fakeTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

describe('applyFlow', () => {
  let invoke = vi.fn();
  let flow: ApplyFlow;
  let spec: EffectSpec;

  beforeEach(() => {
    vi.useFakeTimers();
    invoke = vi.fn().mockResolvedValue(null);
    flow = createApplyFlow(invoke, { timers: fakeTimers });
    spec = defaultSpec();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('runs applying → settling → done on the dip-then-true path', async () => {
    flow.start(MAC, spec);
    expect(flow.getSnapshot()[MAC]).toEqual({ phase: 'applying' });
    expect(invoke).toHaveBeenCalledWith(MAC, spec);

    await vi.advanceTimersByTimeAsync(0); // invoke resolves
    expect(flow.getSnapshot()[MAC]).toEqual({ phase: 'settling' });

    flow.noteStatus(statusWithSync(false)); // upload in progress
    expect(flow.getSnapshot()[MAC]).toEqual({ phase: 'settling' });

    flow.noteStatus(statusWithSync(true)); // confirmed after the dip
    expect(flow.getSnapshot()[MAC]).toEqual({ phase: 'done' });
    expect(flow.lastApplied(MAC)).toEqual(spec);
  });

  it('done auto-clears back to idle after the linger', async () => {
    flow.start(MAC, spec);
    await vi.advanceTimersByTimeAsync(0);
    flow.noteStatus(statusWithSync(false));
    flow.noteStatus(statusWithSync(true));
    expect(flow.getSnapshot()[MAC]?.phase).toBe('done');

    await vi.advanceTimersByTimeAsync(DONE_LINGER_MS);
    expect(flow.getSnapshot()[MAC]).toBeUndefined();
    // last-applied survives the idle transition
    expect(flow.lastApplied(MAC)).toEqual(spec);
  });

  it('does not trust a stale true before the dip, accepts after enough polls', async () => {
    flow.start(MAC, spec);
    await vi.advanceTimersByTimeAsync(0);

    // Stale true from the PREVIOUS effect: polls 1..STALE_TRUE_POLLS ignored.
    for (let i = 0; i < STALE_TRUE_POLLS; i++) {
      flow.noteStatus(statusWithSync(true));
      expect(flow.getSnapshot()[MAC]).toEqual({ phase: 'settling' });
    }
    // One more continuous true — a genuine no-dip confirm (tiny upload).
    flow.noteStatus(statusWithSync(true));
    expect(flow.getSnapshot()[MAC]).toEqual({ phase: 'done' });
  });

  it('fails with the verbatim refusal string', async () => {
    invoke.mockImplementationOnce(() => Promise.reject('radio settling — try again shortly'));
    flow.start(MAC, spec);
    await vi.advanceTimersByTimeAsync(0);
    expect(flow.getSnapshot()[MAC]).toEqual({
      phase: 'failed',
      message: 'radio settling — try again shortly',
    });
    // a failed apply never records last-applied
    expect(flow.lastApplied(MAC)).toBeNull();
  });

  it('times out a settling apply that never confirms', async () => {
    flow.start(MAC, spec);
    await vi.advanceTimersByTimeAsync(0);
    flow.noteStatus(statusWithSync(false));

    await vi.advanceTimersByTimeAsync(APPLY_TIMEOUT_MS);
    const state = flow.getSnapshot()[MAC];
    expect(state?.phase).toBe('failed');
    expect(state && 'message' in state ? state.message : '').toContain('timed out');
  });

  it('ignores duplicate starts while active and other macs in noteStatus', async () => {
    flow.start(MAC, spec);
    flow.start(MAC, { ...spec, speed: 5 }); // ignored — still the first apply
    await vi.advanceTimersByTimeAsync(0);
    expect(invoke).toHaveBeenCalledTimes(1);

    // A status snapshot for a different mac concludes nothing.
    flow.noteStatus(statusWithSync(true, 'aa:bb:cc:dd:ee:ff'));
    expect(flow.getSnapshot()[MAC]).toEqual({ phase: 'settling' });
  });

  it('dismiss clears the op and orphans in-flight callbacks', async () => {
    let resolveInvoke: (v: unknown) => void = () => {};
    invoke.mockImplementationOnce(() => new Promise((r) => (resolveInvoke = r)));
    flow.start(MAC, spec);
    flow.dismiss(MAC);
    expect(flow.getSnapshot()[MAC]).toBeUndefined();

    resolveInvoke(null); // late resolve must not resurrect the entry
    await vi.advanceTimersByTimeAsync(0);
    expect(flow.getSnapshot()[MAC]).toBeUndefined();
    expect(flow.lastApplied(MAC)).toBeNull();
  });

  it('notifies subscribers on every phase change and detaches cleanly', async () => {
    const seen: string[] = [];
    const unsub = flow.subscribe(() => {
      seen.push(flow.getSnapshot()[MAC]?.phase ?? 'idle');
    });
    flow.start(MAC, spec);
    await vi.advanceTimersByTimeAsync(0);
    flow.noteStatus(statusWithSync(false));
    flow.noteStatus(statusWithSync(true));
    expect(seen).toEqual(['applying', 'settling', 'done']);

    unsub();
    await vi.advanceTimersByTimeAsync(DONE_LINGER_MS);
    expect(seen).toEqual(['applying', 'settling', 'done']); // no post-unsub calls
  });

  it('isActiveApply covers exactly the running phases', () => {
    expect(isActiveApply(undefined)).toBe(false);
    expect(isActiveApply({ phase: 'applying' })).toBe(true);
    expect(isActiveApply({ phase: 'settling' })).toBe(true);
    expect(isActiveApply({ phase: 'done' })).toBe(false);
    expect(isActiveApply({ phase: 'failed', message: 'x' })).toBe(false);
  });

  it('a new apply after done replaces last-applied on success', async () => {
    flow.start(MAC, spec);
    await vi.advanceTimersByTimeAsync(0);
    flow.noteStatus(statusWithSync(false));
    flow.noteStatus(statusWithSync(true));
    await vi.advanceTimersByTimeAsync(DONE_LINGER_MS);

    const second: EffectSpec = { ...spec, kind: 'runway', speed: 5 };
    flow.start(MAC, second);
    await vi.advanceTimersByTimeAsync(0);
    flow.noteStatus(statusWithSync(false));
    flow.noteStatus(statusWithSync(true));
    expect(flow.lastApplied(MAC)).toEqual(second);
  });
});

describe('specFromWire (stage)', () => {
  it('fills serde defaults for partial wire specs', async () => {
    const { specFromWire } = await import('./stage.js');
    expect(specFromWire({ kind: 'breathing' })).toEqual({
      kind: 'breathing',
      colors: [],
      speed: 3,
      direction: 'forward',
      brightness: 4,
    });
  });

  it('clamps out-of-range wire values', async () => {
    const { specFromWire } = await import('./stage.js');
    const spec = specFromWire({ kind: 'ripple', speed: 99, brightness: -3, colors: [[300, -5, 20]] });
    expect(spec.speed).toBe(5);
    expect(spec.brightness).toBe(0);
    expect(spec.colors).toEqual([[255, 0, 20]]);
  });
});
