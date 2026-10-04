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
  serverBytes: number;
  serverObjects: number;
  externalObjects: { connectionId: string; objects: number }[];
  unavailableObjects: number;
  evictedBytes: number;
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
    )
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
const selectedCharacterId = () =>
  getDatabase().characters[get(selectedCharID)]?.chaId ?? null;
export const setAssetResidencyPolicy = (policy: AssetResidencyPolicy) =>
  policy === "full"
    ? protectedCommand("server_sync_asset_policy", { policy, selectedCharacterId: selectedCharacterId() })
    : command("server_sync_asset_policy", { policy });
/** Downloads the bodies one external connection holds, or every holder's, keeping the policy. */
export const downloadRemoteAssets = (connectionId?: string) =>
  protectedCommand("asset_residency_download_remote", {
    connectionId: connectionId ?? null,
    selectedCharacterId: selectedCharacterId(),
  });
export const evictLocalAssets = () => protectedCommand("server_sync_asset_evict");
export const cancelAssetResidencyOperation = () =>
  invoke<void>("server_sync_cancel");
