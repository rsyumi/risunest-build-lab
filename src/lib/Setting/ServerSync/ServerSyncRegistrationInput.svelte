<script lang="ts">
  import { onMount, tick } from "svelte";
  import { language } from "src/lang";
  import TextInput from "src/lib/UI/GUI/TextInput.svelte";
  import SettingButton from "../RisuNest/SettingButton.svelte";
  import { isTauriAndroid, isTauriIOS } from "src/ts/platform";
  import { modalNavigation } from "src/ts/ui/modalNavigation";
  import type { ServerConfig } from "src/ts/storage/sync/serverSync";
  import {
    MAX_REGISTRATION_URI_BYTES,
    parseServerRegistration,
    RegistrationError,
  } from "src/ts/storage/sync/serverSyncRegistration";
  import { serverRegistrationInbox } from "src/ts/storage/sync/serverSyncRegistrationInbox";
  import { createServerQrScanner } from "src/ts/storage/sync/serverSyncQr";
  let {
    available = true,
    busy = false,
    message = $bindable(""),
    onRegistration,
  }: {
    available?: boolean;
    busy?: boolean;
    /** What the reader is told about the last code, such as a refused delivery. */
    message?: string;
    onRegistration: (config: ServerConfig) => void;
  } = $props();
  let code = $state("");
  let scanning = $state(false);
  let camera = $state(false);
  let mounted = true;
  const scanner = createServerQrScanner();
  const text = $derived(language.risuNest.serverSync);
  function accept(config: ServerConfig) {
    if (!available || busy) {
      message = text.registrationBlocked;
      return;
    }
    message = "";
    code = "";
    onRegistration(config);
  }
  function read() {
    if (busy || !available) return;
    try {
      accept(parseServerRegistration(code.trim()));
    } catch {
      code = "";
      message = text.registrationInvalid;
    }
  }
  async function scan() {
    if (busy || !available || scanning) return;
    scanning = true;
    message = "";
    try {
      const config = await scanner.scan(() => {
        if (mounted) {
          camera = true;
          document.documentElement.classList.add("risunest-qr-scanning");
        }
      });
      if (mounted) accept(config);
    } catch (error) {
      if (
        mounted &&
        !(
          error instanceof RegistrationError &&
          error.code === "qr-scan-cancelled"
        )
      ) {
        message =
          error instanceof RegistrationError &&
          error.code === "qr-camera-permission-denied"
            ? text.cameraDenied
            : error instanceof RegistrationError &&
                error.code.startsWith("invalid-")
              ? text.registrationInvalid
              : text.cameraUnavailable;
      }
    } finally {
      document.documentElement.classList.remove("risunest-qr-scanning");
      camera = false;
      scanning = false;
    }
  }
  onMount(() => {
    const unsubscribe = serverRegistrationInbox.changed.subscribe(() => {
      void tick().then(() => {
        if (!mounted) return;
        const config = serverRegistrationInbox.take();
        if (config) {
          if (!available || busy) {
            serverRegistrationInbox.releaseConsumed();
            message = text.registrationBlocked;
            return;
          }
          accept(config);
        }
      });
    });
    const hidden = () => {
      if (document.hidden && camera) scanner.cancel();
    };
    document.addEventListener("visibilitychange", hidden);
    return () => {
      mounted = false;
      code = "";
      scanner.cancel();
      serverRegistrationInbox.releaseConsumed();
      unsubscribe();
      document.removeEventListener("visibilitychange", hidden);
      document.documentElement.classList.remove("risunest-qr-scanning");
    };
  });
</script>

{#if available}
  <div class="registration bg-bgcolor">
    <label class="flex flex-col gap-2 font-medium">
      <span>{text.registrationCode}</span>
      <TextInput
        fullwidth
        hideText
        bind:value={code}
        maxlength={MAX_REGISTRATION_URI_BYTES}
        autocapitalize="none"
        spellcheck={false}
        disabled={busy || scanning}
        className="font-mono text-sm"
      />
    </label>
    <p class="text-sm text-textcolor2">{text.registrationCodeHelp}</p>
    <div class="flex flex-wrap gap-2">
      <SettingButton
        disabled={busy || scanning || !code.trim()}
        onclick={read}>{text.readRegistration}</SettingButton
      >
      {#if isTauriAndroid || isTauriIOS}<SettingButton
          variant="secondary"
          disabled={busy || scanning}
          onclick={() => void scan()}>{text.scanRegistration}</SettingButton
        >{/if}
    </div>
  </div>
{/if}
{#if message}<p role="status" class="text-sm">{message}</p>{/if}
{#if scanning}
  <div
    class="scanner-overlay"
    class:camera
    role="dialog"
    aria-modal="true"
    aria-label={text.scanRegistration}
    tabindex="-1"
    use:modalNavigation={{ close: () => scanner.cancel() }}
  >
    <div
      class="scanner-instructions bg-darkbg text-textcolor border border-darkborderc"
    >
      <p>{text.scanRegistrationHelp}</p>
      <SettingButton class="min-h-11" onclick={() => scanner.cancel()}
        >{text.cancelScan}</SettingButton
      >
    </div>
    {#if camera}<div class="scan-frame" aria-hidden="true"></div>{/if}
  </div>
{/if}

<style>
  .registration {
    display: grid;
    gap: 0.85rem;
    padding: 1rem;
    border-radius: 0.375rem;
  }
  .scanner-overlay {
    position: fixed;
    inset: 0;
    z-index: 2147483000;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: space-between;
    gap: 2rem;
    padding: max(1rem, env(safe-area-inset-top)) 1rem
      max(2rem, env(safe-area-inset-bottom));
    background: var(--risu-theme-bgcolor);
  }
  .scanner-overlay.camera {
    background: transparent;
  }
  .scanner-instructions {
    display: grid;
    gap: 1rem;
    padding: 1rem;
    border-radius: 0.5rem;
    width: min(100%, 28rem);
  }
  .scan-frame {
    width: min(75vw, 55vh);
    aspect-ratio: 1;
    border: 3px solid white;
    border-radius: 1rem;
    margin: auto;
    box-shadow: 0 0 0 2px #0008;
  }
  :global(html.risunest-qr-scanning),
  :global(html.risunest-qr-scanning body) {
    background: transparent !important;
  }
  :global(html.risunest-qr-scanning body *) {
    visibility: hidden;
  }
  :global(html.risunest-qr-scanning .scanner-overlay),
  :global(html.risunest-qr-scanning .scanner-overlay *) {
    visibility: visible;
  }
</style>
