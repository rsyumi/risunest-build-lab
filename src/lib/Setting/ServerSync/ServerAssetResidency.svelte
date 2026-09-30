<script lang="ts">
  import { onMount } from "svelte";
  import { language } from "src/lang";
  import SegmentedButtons from "../RisuNest/SegmentedButtons.svelte";
  import SettingRow from "../RisuNest/SettingRow.svelte";
  import SettingButton from "../RisuNest/SettingButton.svelte";
  import { formatRisuNestStorageBytes as bytes } from "src/ts/storage/risuNestStorageDashboard";
  import { serverSyncError } from "src/ts/storage/sync/serverSync";
  import { serverSyncErrorHelp } from "src/ts/storage/sync/serverSyncConnectFlow";
  import {
    getAssetResidencyStatus,
    setAssetResidencyPolicy,
    evictLocalAssets,
    cancelAssetResidencyOperation,
    type AssetResidencyPolicy,
    type AssetResidencyStatus,
  } from "src/ts/storage/sync/serverAssetResidency";
  import { reserveLibraryFileOperation } from "src/ts/storage/libraryFileOperation";
  let { disabled = false, running = $bindable(null) }: { disabled?: boolean; running?: "policy" | "evict" | null } = $props();
  let status = $state<AssetResidencyStatus>();
  let loading = $state(false);
  const busy = $derived(running !== null);
  let error = $state("");
  let freed = $state(0);
  const text = $derived(language.risuNest.serverSync.residency);
  const policy = $derived<AssetResidencyPolicy>(status?.policy ?? "full");
  const options = $derived([
    { value: "full" as const, label: text.full },
    { value: "remote" as const, label: text.remote },
  ]);
  async function run(
    action: () => Promise<AssetResidencyStatus>,
    kind: "policy" | "evict" = "policy",
  ) {
    if (busy) return;
    running = kind;
    error = "";
    freed = 0;
    let release: (() => void) | undefined;
    try {
      release = reserveLibraryFileOperation();
      status = await action();
      freed = status.evictedBytes;
    } catch (cause) {
      error = !release && cause instanceof Error && cause.message === "library-file-operation-busy"
        ? "library-operation-busy" : serverSyncError(cause).code;
      try {
        status = await getAssetResidencyStatus();
      } catch {
        /* Keep the last known counts. */
      }
    } finally {
      running = null;
      release?.();
    }
  }
  /** Reading the counts is not an operation the row can cancel. */
  async function load(): Promise<void> {
    loading = true;
    try {
      status = await getAssetResidencyStatus();
    } catch (cause) {
      error = serverSyncError(cause).code;
    } finally {
      loading = false;
    }
  }
  onMount(() => {
    void load();
  });
</script>

<SettingRow label={text.title} help={text.description} aria-busy={busy}>
  {#snippet below()}
    {#if status}
      <div class="mt-2 flex flex-wrap gap-x-3.5 gap-y-1 text-[13px] text-textcolor2 tabular-nums">
        <span>{text.local} {bytes(status.localBytes)}</span>
        <span>{text.remoteOnly} {bytes(status.remoteBytes)} ({status.remoteObjects.toLocaleString()})</span>
      </div>
      {#if status.unavailableObjects > 0}<p role="alert" class="mt-1 text-sm text-danger-400">
          {text.unavailable}: {status.unavailableObjects.toLocaleString()}
        </p>{/if}
      {#if status.policy === "remote"}<p class="mt-1 text-[13px] text-textcolor2">
          {text.cleanupNote}
        </p>{/if}
    {/if}
    {#if busy}<p role="status" class="mt-1 text-sm">{text.working}</p>{/if}
    {#if freed > 0}<p role="status" class="mt-1 text-sm">
        {text.freed}: {bytes(freed)}
      </p>{/if}
    {#if error}<p role="alert" class="mt-1 text-sm text-danger-400">
        {error === "library-operation-busy" || error === "local-storage-full"
          ? serverSyncErrorHelp(error, language.risuNest.serverSync)
          : language.risuNest.storage.actionFailed} ({error})
      </p>{/if}
  {/snippet}
  <SegmentedButtons
    value={policy}
    {options}
    label={text.title}
    role="radiogroup"
    disabled={busy || loading || disabled}
    onchange={(next) => run(() => setAssetResidencyPolicy(next))}
  />
  <SettingButton
    variant="secondary"
    busy={running === "evict"}
    disabled={busy || loading || disabled || policy !== "remote"}
    onclick={() => run(evictLocalAssets, "evict")}>{text.clean}</SettingButton
  >
  {#if status?.policy === "full" && status.remoteObjects > 0}
    <SettingButton disabled={busy || loading || disabled} onclick={() => run(() => setAssetResidencyPolicy("full"))}>{text.download}</SettingButton>
  {/if}
  {#if busy}<SettingButton
      variant="secondary"
      onclick={() =>
        cancelAssetResidencyOperation().catch((cause) => {
          error = serverSyncError(cause).code;
        })}>{text.cancel}</SettingButton
    >{/if}
</SettingRow>
