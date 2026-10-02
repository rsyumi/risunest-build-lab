import { invoke } from "@tauri-apps/api/core";
import { beginMobileBackgroundTask } from "../../mobileBackgroundTask";
import { get } from "svelte/store";
import { selectedCharID } from "src/ts/stores.svelte";
import { getDatabase } from "../database.svelte";

export type AssetResidencyPolicy = "full" | "remote";
export interface AssetResidencyStatus {
  policy: AssetResidencyPolicy;
  localBytes: number;
  remoteBytes: number;
  remoteObjects: number;
  unavailableObjects: number;
  evictedBytes: number;
}
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
      result.unavailableObjects,
      result.evictedBytes,
    ].some((value) => !Number.isSafeInteger(value) || value < 0)
  ) {
    throw new Error("invalid-asset-residency-status");
  }
  return result;
}
export const getAssetResidencyStatus = () =>
  command("server_sync_asset_status");
async function protectedCommand(name: string, args?: Record<string, unknown>) {
  const task = await beginMobileBackgroundTask("sync");
  let cancellation: Promise<void> | undefined;
  const cancel = () => {
    cancellation ??= cancelAssetResidencyOperation().catch(() => {});
  };
  task.signal?.addEventListener("abort", cancel, { once: true });
  try {
    if (task.signal?.aborted) throw task.signal.reason;
    task.progress(null);
    return await command(name, args);
  } finally {
    task.signal?.removeEventListener("abort", cancel);
    await cancellation;
    await task.dispose();
  }
}
export const setAssetResidencyPolicy = (policy: AssetResidencyPolicy) =>
  policy === "full"
    ? protectedCommand("server_sync_asset_policy", { policy, selectedCharacterId: getDatabase().characters[get(selectedCharID)]?.chaId ?? null })
    : command("server_sync_asset_policy", { policy });
export const evictLocalAssets = () => protectedCommand("server_sync_asset_evict");
export const cancelAssetResidencyOperation = () =>
  invoke<void>("server_sync_cancel");
