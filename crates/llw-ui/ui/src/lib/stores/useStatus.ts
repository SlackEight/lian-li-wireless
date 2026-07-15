/**
 * React binding for the status store: the app-wide singleton wired to Tauri's
 * `invoke`, plus the `useStatus()` hook. The store itself lives in status.ts
 * (framework-free, tested with fakes).
 *
 * Lifecycle is ref-counted: the first mounted consumer starts polling and the
 * last unmount stops it, so any component can call `useStatus()` without
 * coordinating ownership.
 */
import { useEffect, useSyncExternalStore } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { createStatusStore, type StatusSnapshot } from './status.js';

export const statusStore = createStatusStore((cmd) => invoke(cmd));

let consumers = 0;

export function useStatus(): StatusSnapshot {
  useEffect(() => {
    consumers += 1;
    if (consumers === 1) statusStore.start();
    return () => {
      consumers -= 1;
      if (consumers === 0) statusStore.stop();
    };
  }, []);
  return useSyncExternalStore(statusStore.subscribe, statusStore.getSnapshot);
}
