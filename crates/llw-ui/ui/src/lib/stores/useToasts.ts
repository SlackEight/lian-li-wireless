/**
 * React binding for the toast store: app-wide singleton + hook. The store
 * itself lives in toasts.ts (framework-free, tested with fakes).
 */
import { useSyncExternalStore } from 'react';
import { createToastStore, type Toast } from './toasts.js';

export const toastStore = createToastStore();

export function useToasts(): readonly Toast[] {
  return useSyncExternalStore(toastStore.subscribe, toastStore.getSnapshot);
}
