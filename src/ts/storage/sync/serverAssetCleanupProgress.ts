import { writable } from 'svelte/store';

export type AssetCleanupStage = 'preparing' | 'checking' | 'uploading' | 'deleting' | 'finalizing';
export interface AssetCleanupSnapshot {
  sequence: number;
  stage: AssetCleanupStage;
  completedItems: number;
  totalItems: number | null;
  evictedItems: number;
  evictedBytes: number;
}
export interface AssetCleanupState extends AssetCleanupSnapshot {
  operationId: string;
  state: 'running' | 'cancelling' | 'complete' | 'cancelled' | 'failed';
}

export const assetCleanupProgress = writable<AssetCleanupState | undefined>();
export const isAssetCleanupRunning = (value?: AssetCleanupState) => value?.state === 'running' || value?.state === 'cancelling';

export function validAssetCleanupSnapshot(value: AssetCleanupSnapshot): boolean {
  const count = (value: unknown) => Number.isSafeInteger(value) && (value as number) >= 0;
  return !!value && ['preparing', 'checking', 'uploading', 'deleting', 'finalizing'].includes(value.stage)
    && [value.sequence, value.completedItems, value.evictedItems, value.evictedBytes].every(count)
    && (value.totalItems === null || (count(value.totalItems) && value.completedItems <= value.totalItems));
}
