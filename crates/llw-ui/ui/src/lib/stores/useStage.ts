/**
 * React binding for the stage store singleton (useStatus idiom, minus the
 * lifecycle — the store is passive). Lives outside Lighting.tsx so the
 * effect rail can share it without an import cycle.
 */
import { useSyncExternalStore } from 'react';
import { stageStore, type StageSnapshot } from './stage.js';

export function useStage(): StageSnapshot {
  return useSyncExternalStore(stageStore.subscribe, stageStore.getSnapshot);
}
