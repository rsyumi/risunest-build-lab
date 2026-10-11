import { Channel, invoke } from "@tauri-apps/api/core";
import { beginExternalProgress, externalProgressFailure } from './external/progress';
import { beginMobileBackgroundTask } from "../../mobileBackgroundTask";
import { get } from "svelte/store";
import { selectedCharID } from "src/ts/stores.svelte";
import { getDatabase } from "../database.svelte";
import { registerLibraryFileOperationGate } from '../libraryFileOperation';
import { assetCleanupProgress, validAssetCleanupSnapshot, type AssetCleanupSnapshot } from './serverAssetCleanupProgress';

const operations = new Set<Promise<unknown>>();
registerLibraryFileOperationGate(async () => {
  while (operations.size) await Promise.allSettled([...operations]);
  assetCleanupProgress.set(undefined);
});

export type AssetResidencyTarget = { kind: 'external'; connectionId: string } | { kind: 'server'; libraryId: string; targetId: string };

export type AssetResidencyPolicy = "full" | "remote";
export interface AssetResidencyStatus {
  policy: AssetResidencyPolicy;
  localBytes: number;
  remoteBytes: number;
  remoteObjects: number;
  serverBytes: number;
  serverObjects: number;
  externalObjects: { connectionId: string; objects: number }[];
  unavailableObjects: number;
  evictedBytes: number;
  previousStorageObjects?: number;
}
const count = (value: unknown) =>
  Number.isSafeInteger(value) && (value as number) >= 0;
async function command(
  name: string,
  args?: Record<string, unknown>,
): Promise<AssetResidencyStatus> {
  const result = await invoke<AssetResidencyStatus>(name, args);
  if (
    !result ||
    !["full", "remote"].includes(result.policy) ||
    [
      result.localBytes,
      result.remoteBytes,
      result.remoteObjects,
      result.serverBytes,
      result.serverObjects,
      result.unavailableObjects,
      result.evictedBytes,
    ].some((value) => !count(value)) ||
    !Array.isArray(result.externalObjects) ||
    result.externalObjects.some(
      (entry) =>
        !entry ||
        typeof entry.connectionId !== "string" ||
        !entry.connectionId ||
        !count(entry.objects),
    ) ||
    (result.previousStorageObjects !== undefined && !count(result.previousStorageObjects)) ||
    (name === "server_sync_asset_status" && args?.target !== undefined && !count(result.previousStorageObjects))
  ) {
    throw new Error("invalid-asset-residency-status");
  }
  return result;
}
export const getAssetResidencyStatus = (target?: AssetResidencyTarget) =>
  command("server_sync_asset_status", target ? { target } : undefined);
/** The status's `externalObjects` count for one connection, without the rest of the status. */
export async function countConnectionOnlyAssets(connectionId: string): Promise<number> {
  const objects = await invoke<number>("asset_residency_connection_objects", { connectionId });
  if (!count(objects)) throw new Error("invalid-asset-residency-status");
  return objects;
}
function protectedCommand(name: string, args?: Record<string, unknown>, signal?: AbortSignal, operationId?: string) {
  const operation = runProtectedCommand(name, args, signal, operationId);
  operations.add(operation);
  return operation.finally(() => { operations.delete(operation); });
}
async function runProtectedCommand(name: string, args?: Record<string, unknown>, signal?: AbortSignal, operationId?: string) {
  const task = await beginMobileBackgroundTask("sync");
  let cancellation: Promise<void> | undefined;
  let admitted = operationId === undefined;
  const cancel = () => {
    if (admitted) cancellation ??= cancelAssetResidencyOperation(operationId).catch(() => {});
  };
  task.signal?.addEventListener("abort", cancel, { once: true });
  signal?.addEventListener("abort", cancel, { once: true });
  try {
    if (task.signal?.aborted) throw task.signal.reason;
    signal?.throwIfAborted();
    if (operationId !== undefined) {
      await invoke<void>("asset_residency_prepare_download", { operationId });
      admitted = true;
      if (signal?.aborted || task.signal?.aborted) {
        cancel();
        signal?.throwIfAborted();
        throw task.signal!.reason;
      }
    }
    task.progress(null);
    const result = await command(name, args);
    signal?.throwIfAborted();
    if (task.signal?.aborted) throw task.signal.reason;
    return result;
  } finally {
    task.signal?.removeEventListener("abort", cancel);
    signal?.removeEventListener("abort", cancel);
    if (operationId !== undefined) cancel();
    await cancellation;
    await task.dispose();
  }
}
const selectedCharacterId = () =>
  getDatabase().characters[get(selectedCharID)]?.chaId ?? null;
export const setAssetResidencyPolicy = (policy: AssetResidencyPolicy) =>
  policy === "full"
    ? protectedCommand("server_sync_asset_policy", { policy, selectedCharacterId: selectedCharacterId() })
    : command("server_sync_asset_policy", { policy });
/** Downloads connection-exclusive bodies, every holder's bodies, or files absent from a target. */
export const downloadRemoteAssets = async (connectionId?: string, options?: { signal?: AbortSignal; target?: AssetResidencyTarget; showEmpty?: boolean; progressConnectionId?: string }) => {
  const operationId = crypto.randomUUID();
  const externalId = connectionId ?? (options?.target?.kind === 'external' ? options.target.connectionId : options?.progressConnectionId);
  const run = externalId ? beginExternalProgress(externalId, 'download', options?.showEmpty ?? true) : undefined;
  let hasItems = false;
  const progress = run ? new Channel<{ completedItems: number; totalItems: number }>() : undefined;
  if (progress) progress.onmessage = value => {
    if (count(value.completedItems) && count(value.totalItems) && value.completedItems <= value.totalItems) {
      hasItems ||= value.totalItems > 0;
      if (value.totalItems > 0 || options?.showEmpty !== false) run?.items(value.completedItems, value.totalItems);
    }
  };
  try {
    const result = await protectedCommand("asset_residency_download_remote", {
      connectionId: connectionId ?? null,
      selectedCharacterId: selectedCharacterId(),
      operationId,
      ...(options?.target ? { target: options.target } : {}),
      ...(progress ? { progress, transferProgress: run!.networkChannel() } : {}),
    }, options?.signal, operationId);
    run?.finish('complete', hasItems || options?.showEmpty !== false);
    return result;
  } catch (error) { run?.finish(externalProgressFailure(error)); throw error; }
};
let cleanup: { operationId: string; controller: AbortController } | undefined;
export async function evictLocalAssets(): Promise<AssetResidencyStatus> {
  if (cleanup) throw new Error('asset-cleanup-busy');
  const operationId = crypto.randomUUID();
  const controller = new AbortController();
  cleanup = { operationId, controller };
  assetCleanupProgress.set({ operationId, state: 'running', sequence: 0, stage: 'preparing', completedItems: 0, totalItems: null, evictedItems: 0, evictedBytes: 0 });
  const progress = new Channel<AssetCleanupSnapshot>();
  progress.onmessage = value => {
    if (!validAssetCleanupSnapshot(value)) return;
    // The final channel message can arrive after invoke rejects. Preserve the
    // terminal state while accepting this operation's last partial counters.
    assetCleanupProgress.update(current => current?.operationId === operationId
      && current.state !== 'complete' && value.sequence > current.sequence
      ? { ...current, ...value } : current);
  };
  try {
    const result = await protectedCommand('server_sync_asset_evict', { operationId, progress }, controller.signal, operationId);
    assetCleanupProgress.update(current => current?.operationId === operationId ? { ...current, state: 'complete', evictedBytes: result.evictedBytes } : current);
    return result;
  } catch (error) {
    const cancelled = controller.signal.aborted || (typeof error === 'object' && error !== null
      && (('code' in error && error.code === 'cancelled') || ('name' in error && error.name === 'AbortError')));
    assetCleanupProgress.update(current => current?.operationId === operationId ? { ...current, state: cancelled ? 'cancelled' : 'failed' } : current);
    throw error;
  } finally {
    if (cleanup?.operationId === operationId) cleanup = undefined;
  }
}
export function cancelLocalAssetCleanup(): void {
  if (!cleanup) return;
  assetCleanupProgress.update(current => current?.operationId === cleanup?.operationId ? { ...current, state: 'cancelling' } : current);
  cleanup.controller.abort();
}
export const cancelAssetResidencyOperation = (operationId?: string) =>
  invoke<void>("server_sync_cancel", operationId ? { operationId } : undefined);
