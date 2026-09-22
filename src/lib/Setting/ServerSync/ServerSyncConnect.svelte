<script lang="ts">
  import { ChevronDownIcon, ChevronRightIcon } from "@lucide/svelte";
  import { language } from "src/lang";
  import TextInput from "src/lib/UI/GUI/TextInput.svelte";
  import SegmentedButtons from "../RisuNest/SegmentedButtons.svelte";
  import SettingButton from "../RisuNest/SettingButton.svelte";
  import ServerSyncRegistrationInput from "./ServerSyncRegistrationInput.svelte";
  import type { AssetResidencyPolicy } from "src/ts/storage/sync/serverAssetResidency";
  import type { ServerConfig } from "src/ts/storage/sync/serverSync";
  import { serverSyncError } from "src/ts/storage/sync/serverSync";
  import type { ServerSyncConnectRequest } from "src/ts/storage/sync/serverSyncConnectFlow";
  import { serverSyncErrorHelp } from "src/ts/storage/sync/serverSyncConnectFlow";
  import {
    validateServerSyncEndpoint,
    validateServerSyncId,
  } from "src/ts/storage/sync/serverSyncConnection";
  import type { ServerSyncNavigation } from "src/ts/storage/sync/serverSyncDeepLink";
  import { serverRegistrationInbox } from "src/ts/storage/sync/serverSyncRegistrationInbox";

  /**
   * Registration code, then the server check. Both the onboarding and the
   * settings place it; the parent runs the connection and reports its error.
   */
  let {
    stage = $bindable("code"),
    initialNavigation,
    available = true,
    busy = false,
    replacing = false,
    error = "",
    tone = "settings",
    onSubmit,
  }: {
    stage?: "code" | "review";
    initialNavigation?: ServerSyncNavigation;
    /** False while another server is bound; a delivered code is then refused. */
    available?: boolean;
    busy?: boolean;
    replacing?: boolean;
    /** The parent's last connection error code. */
    error?: string;
    tone?: "settings" | "onboarding";
    onSubmit: (request: ServerSyncConnectRequest) => void;
  } = $props();
  const text = $derived(language.risuNest.serverSync);
  let config = $state<ServerConfig | undefined>();
  let residency = $state<AssetResidencyPolicy>("full");
  let endpoint = $state("");
  let libraryId = $state("");
  let deviceId = $state("");
  let token = $state("");
  let manualOpen = $state(false);
  let formError = $state("");
  let inputRevision = $state(0);
  let appliedNavigation: ServerSyncNavigation | undefined;
  $effect(() => {
    if (!initialNavigation || appliedNavigation === initialNavigation) return;
    appliedNavigation = initialNavigation;
    if (!available || busy || stage !== "code") return;
    endpoint = initialNavigation.endpoint ?? "";
    libraryId = initialNavigation.libraryId ?? "";
    deviceId = "";
    token = "";
    manualOpen = Boolean(endpoint || libraryId);
  });
  const residencyOptions = $derived([
    { value: "full" as const, label: text.residency.full },
    { value: "remote" as const, label: text.residency.remote },
  ]);
  function accept(next: ServerConfig): void {
    if (!available || busy) return;
    // Native binding is authoritative; the address is only normalized here.
    config = { ...next, endpoint: validateServerSyncEndpoint(next.endpoint) };
    formError = "";
    stage = "review";
  }
  function readManualEntry(): void {
    if (busy) return;
    formError = "";
    try {
      accept({
        endpoint: validateServerSyncEndpoint(endpoint.trim()),
        libraryId: validateServerSyncId(libraryId.trim()),
        deviceId: validateServerSyncId(deviceId.trim()),
        token: token.trim(),
      });
    } catch (cause) {
      formError = serverSyncError(cause).code;
    }
  }
  function discard(): void {
    config = undefined;
    endpoint = "";
    libraryId = "";
    deviceId = "";
    token = "";
    manualOpen = false;
    formError = "";
    serverRegistrationInbox.clear();
    inputRevision++;
    stage = "code";
  }
  function submit(): void {
    if (busy || !config) return;
    onSubmit({
      config: $state.snapshot(config),
      residency,
      ...(replacing ? { replacing: true } : {}),
    });
  }
</script>

<div class="connect text-textcolor">
  {#if stage === "code"}
    {#key inputRevision}
      <ServerSyncRegistrationInput {available} {busy} onRegistration={accept} />
    {/key}
    {#if available}
      <div class="manual">
        <button
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md text-sm text-textcolor2 transition-colors duration-200 hover:text-textcolor focus:outline-hidden focus-visible:ring-2 focus-visible:ring-selected"
          aria-expanded={manualOpen}
          onclick={() => {
            manualOpen = !manualOpen;
          }}
        >
          {#if manualOpen}<ChevronDownIcon size={16} aria-hidden="true" />{:else}<ChevronRightIcon
              size={16}
              aria-hidden="true"
            />{/if}
          <span>{text.manualEntry}</span>
        </button>
        <form
          class="fields"
          hidden={!manualOpen}
          onsubmit={(event) => {
            event.preventDefault();
            readManualEntry();
          }}
        >
          <p class="text-sm text-textcolor2">{text.credentialsHelp}</p>
          <label class="field"
            ><span>{text.endpoint}</span><TextInput
              fullwidth
              bind:value={endpoint}
              placeholder={text.endpointPlaceholder}
              disabled={busy}
              className="disabled:opacity-50"
            /></label
          >
          <div class="identity">
            <label class="field"
              ><span>{text.libraryId}</span><TextInput
                fullwidth
                bind:value={libraryId}
                disabled={busy}
                className="disabled:opacity-50"
              /></label
            >
            <label class="field"
              ><span>{text.deviceId}</span><TextInput
                fullwidth
                bind:value={deviceId}
                disabled={busy}
                className="disabled:opacity-50"
              /></label
            >
          </div>
          <label class="field"
            ><span>{text.token}</span><TextInput
              fullwidth
              hideText
              bind:value={token}
              disabled={busy}
              className="disabled:opacity-50"
            /></label
          >
          {#if formError}<p class="text-sm text-danger-400" role="alert">
              {text.credentialsHelp} <span class="text-textcolor2">({formError})</span>
            </p>{/if}
          <div class="actions">
            <SettingButton type="submit" {busy}>{text.reviewTitle}</SettingButton>
            <SettingButton variant="secondary" disabled={busy} onclick={discard}
              >{text.discardRegistration}</SettingButton
            >
          </div>
        </form>
      </div>
    {/if}
  {:else if config}
    <form
      class="review-form"
      onsubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      {#if tone === "settings"}<p class="text-sm text-textcolor2">
          {text.reviewLead}
        </p>{/if}
      <dl class="review">
        <dt>{text.endpoint}</dt>
        <dd>{config.endpoint}</dd>
        <dt>{text.libraryId}</dt>
        <dd>{config.libraryId}</dd>
        <dt>{text.deviceId}</dt>
        <dd>{config.deviceId}</dd>
        <dt>{text.token}</dt>
        <dd aria-label={text.token}>••••••••</dd>
        {#if config.directory}
          <dt>{text.directoryEnabled}</dt>
          <dd>{config.directory.baseUrl}</dd>
        {/if}
      </dl>
      <div class="policy">
        <div class="min-w-0">
          <div class="text-[15px]">{text.residency.title}</div>
          <p class="policy-help mt-0.5 max-w-[62ch] text-[13px] leading-normal">
            {text.residency.description}
          </p>
        </div>
        <SegmentedButtons
          bind:value={residency}
          options={residencyOptions}
          label={text.residency.title}
          role="radiogroup"
          disabled={busy}
        />
      </div>
      <div class="actions">
        <SettingButton type="submit" {busy}
          >{replacing ? text.reregister : text.connect}</SettingButton
        >
        <SettingButton variant="secondary" disabled={busy} onclick={discard}
          >{text.otherCode}</SettingButton
        >
      </div>
      <p class="text-sm text-textcolor2">{text.connectHint}</p>
    </form>
  {/if}
  {#if error && error !== "cancelled"}
    <p class="text-sm text-danger-400" role="alert">
      {serverSyncErrorHelp(error, text)} <span class="text-textcolor2">({error})</span>
    </p>
  {/if}
</div>

<style>
  .connect {
    container-type: inline-size;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    min-width: 0;
    gap: 0.85rem;
    overflow-wrap: anywhere;
  }
  .fields,
  .review-form,
  .field {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 0.5rem;
  }
  .fields {
    gap: 0.85rem;
    margin-top: 0.75rem;
  }
  .review-form {
    gap: 0.85rem;
  }
  .field > span {
    font-size: 0.875rem;
    font-weight: 500;
  }
  .identity {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 0.85rem;
  }
  .review {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 0.15rem 1.1rem;
    margin: 0;
    padding: 0.85rem 1rem;
    /* The onboarding's confirmation card tint. */
    border: 1px solid color-mix(in srgb, var(--risu-theme-primary-500) 35%, transparent);
    border-radius: 0.5rem;
    background: color-mix(in srgb, var(--risu-theme-primary-500) 8%, transparent);
    font-size: 0.85rem;
  }
  .review dt {
    font-size: 0.78rem;
    color: var(--risu-theme-textcolor2);
  }
  .review dd {
    margin: 0 0 0.5rem;
    min-width: 0;
    font-weight: 600;
  }
  .review dd:last-child {
    margin-bottom: 0;
  }
  .policy {
    display: grid;
    gap: 0.5rem;
    padding: 0.75rem 0.85rem;
    border: 1px solid var(--risu-theme-darkborderc);
    border-radius: 0.5rem;
  }
  .policy-help {
    color: color-mix(in srgb, var(--risu-theme-textcolor2) 62%, var(--risu-theme-textcolor) 38%);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  @container (min-width: 480px) {
    .identity {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .review {
      grid-template-columns: auto minmax(0, 1fr);
      gap: 0.5rem 1.1rem;
    }
    .review dd {
      margin: 0;
    }
  }
</style>
