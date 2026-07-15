import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { createToastStore, TOAST_DISMISS_MS } from './toasts.js';

describe('toast store', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('push shows a toast; each gets a unique id', () => {
    const store = createToastStore();
    const a = store.push('error', 'refusing to bind: device is bound to another master');
    const b = store.push('warn', 'radio settling');
    expect(a).not.toBe(b);
    expect(store.getSnapshot()).toEqual([
      { id: a, kind: 'error', message: 'refusing to bind: device is bound to another master' },
      { id: b, kind: 'warn', message: 'radio settling' },
    ]);
  });

  it('auto-dismisses after 6s', () => {
    const store = createToastStore();
    store.push('error', 'boom');
    vi.advanceTimersByTime(TOAST_DISMISS_MS - 1);
    expect(store.getSnapshot()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(store.getSnapshot()).toHaveLength(0);
  });

  it('manual dismiss removes just that toast and cancels its timer', () => {
    const store = createToastStore();
    const onChange = vi.fn();
    store.subscribe(onChange);
    const a = store.push('error', 'first');
    const b = store.push('error', 'second');

    store.dismiss(a);
    expect(store.getSnapshot().map((t) => t.id)).toEqual([b]);

    const calls = onChange.mock.calls.length;
    store.dismiss(a); // unknown id → no-op, no notify
    vi.advanceTimersByTime(TOAST_DISMISS_MS * 2); // a's timer must not double-fire
    expect(store.getSnapshot()).toHaveLength(0); // only b auto-dismissed
    expect(onChange.mock.calls.length).toBe(calls + 1);
  });

  it('getSnapshot is referentially stable between changes', () => {
    const store = createToastStore();
    expect(store.getSnapshot()).toBe(store.getSnapshot());
    store.push('warn', 'x');
    expect(store.getSnapshot()).toBe(store.getSnapshot());
  });
});
