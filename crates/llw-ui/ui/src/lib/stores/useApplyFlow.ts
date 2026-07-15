/**
 * React binding for the apply-flow store: the app-wide singleton wired to
 * Tauri's `invoke`, fed by the status store's polls, plus the
 * `useApplyFlow()` hook. The state machine itself lives in applyFlow.ts
 * (framework-free, tested with fakes).
 */
import { useSyncExternalStore } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { createApplyFlow, type ApplyFlowSnapshot } from './applyFlow.js';
import { statusStore } from './useStatus.js';
import { toastStore } from './useToasts.js';

export const applyFlow = createApplyFlow((mac, spec) => invoke('set_effect', { mac, spec }));

// Every status poll feeds the flow so settling applies can conclude.
statusStore.subscribe(() => {
  const { data } = statusStore.getSnapshot();
  if (data) applyFlow.noteStatus(data);
});

// Failures surface as toasts (daemon refusal strings verbatim); the apply
// then clears back to idle so the Apply button is immediately available.
let seenPhases = new Map<string, string>();
applyFlow.subscribe(() => {
  const snap = applyFlow.getSnapshot();
  const phases = new Map<string, string>();
  for (const [mac, state] of Object.entries(snap)) {
    phases.set(mac, state.phase);
    if (state.phase === 'failed' && seenPhases.get(mac) !== 'failed') {
      toastStore.push('error', state.message);
      queueMicrotask(() => applyFlow.dismiss(mac));
    }
  }
  seenPhases = phases;
});

export function useApplyFlow(): ApplyFlowSnapshot {
  return useSyncExternalStore(applyFlow.subscribe, applyFlow.getSnapshot);
}
