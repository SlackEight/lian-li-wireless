/**
 * React binding for the bind-flow store: the app-wide singleton wired to
 * Tauri's `invoke`, fed by the status store's polls, plus the `useBindFlow()`
 * hook. The state machine itself lives in bindFlow.ts (framework-free,
 * tested with fakes).
 */
import { useSyncExternalStore } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { createBindFlow, type BindFlowSnapshot } from './bindFlow.js';
import { statusStore } from './useStatus.js';
import { toastStore } from './useToasts.js';

export const bindFlow = createBindFlow((op, args) => invoke(op, args));

// Every status poll feeds the flow so converging ops can conclude.
statusStore.subscribe(() => {
  const { data } = statusStore.getSnapshot();
  if (data) bindFlow.noteStatus(data);
});

// Failures surface as toasts (daemon refusal strings verbatim); the op then
// clears back to idle so its Bind/Unbind control is immediately available.
let seenPhases = new Map<string, string>();
bindFlow.subscribe(() => {
  const snap = bindFlow.getSnapshot();
  const phases = new Map<string, string>();
  for (const [mac, state] of Object.entries(snap)) {
    phases.set(mac, state.phase);
    if (state.phase === 'failed' && seenPhases.get(mac) !== 'failed') {
      toastStore.push('error', state.message);
      queueMicrotask(() => bindFlow.dismiss(mac));
    }
  }
  seenPhases = phases;
});

export function useBindFlow(): BindFlowSnapshot {
  return useSyncExternalStore(bindFlow.subscribe, bindFlow.getSnapshot);
}
