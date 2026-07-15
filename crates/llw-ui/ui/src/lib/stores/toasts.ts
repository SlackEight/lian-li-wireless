/**
 * Toast store — a minimal queue for transient notices (daemon refusal
 * strings, rename failures). Auto-dismisses after 6s; click dismisses early.
 *
 * Framework-free like status.ts: Vitest drives it with fake timers; React
 * consumes it through useToasts.ts.
 */
import type { Timers } from './status.js';

/** How long a toast lingers before dismissing itself. */
export const TOAST_DISMISS_MS = 6000;

export type ToastKind = 'error' | 'warn';

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

export interface ToastStore {
  getSnapshot(): readonly Toast[];
  subscribe(onChange: () => void): () => void;
  /** Show a toast; returns its id (auto-dismissed after TOAST_DISMISS_MS). */
  push(kind: ToastKind, message: string): number;
  dismiss(id: number): void;
}

const defaultTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export function createToastStore(opts: { timers?: Timers } = {}): ToastStore {
  const timers = opts.timers ?? defaultTimers;
  let snapshot: readonly Toast[] = [];
  const listeners = new Set<() => void>();
  const autoDismiss = new Map<number, unknown>();
  let nextId = 1;

  function setSnapshot(next: readonly Toast[]) {
    snapshot = next;
    for (const cb of listeners) cb();
  }

  function dismiss(id: number) {
    const timer = autoDismiss.get(id);
    if (timer !== undefined) {
      timers.clear(timer);
      autoDismiss.delete(id);
    }
    if (snapshot.some((t) => t.id === id)) {
      setSnapshot(snapshot.filter((t) => t.id !== id));
    }
  }

  return {
    getSnapshot: () => snapshot,

    subscribe(onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },

    push(kind, message) {
      const id = nextId++;
      autoDismiss.set(
        id,
        timers.set(() => {
          autoDismiss.delete(id);
          dismiss(id);
        }, TOAST_DISMISS_MS),
      );
      setSnapshot([...snapshot, { id, kind, message }]);
      return id;
    },

    dismiss,
  };
}
