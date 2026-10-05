import { invoke } from "@tauri-apps/api/core";
import { beginMobileBackgroundTask } from "../../mobileBackgroundTask";
import { get } from "svelte/store";
import { selectedCharID } from "src/ts/stores.svelte";
import { getDatabase } from "../database.svelte";

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
async function protectedCommand(name: string, args?: Record<string, unknown>, signal?: AbortSignal, operationId?: string) {
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
/** Downloads the bodies one external connection holds, or every holder's, keeping the policy. */
export const downloadRemoteAssets = (connectionId?: string, options?: { signal?: AbortSignal; target?: AssetResidencyTarget }) => {
  const operationId = crypto.randomUUID();
  return protectedCommand("asset_residency_download_remote", {
    connectionId: connectionId ?? null,
    selectedCharacterId: selectedCharacterId(),
    operationId,
    ...(options?.target ? { target: options.target } : {}),
  }, options?.signal, operationId);
};
export const evictLocalAssets = () => protectedCommand("server_sync_asset_evict");
export const cancelAssetResidencyOperation = (operationId?: string) =>
  invoke<void>("server_sync_cancel", operationId ? { operationId } : undefined);
