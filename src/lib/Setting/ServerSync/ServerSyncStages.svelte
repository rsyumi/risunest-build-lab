<script lang="ts">
  import { Check, Circle, LoaderCircle } from "@lucide/svelte";
  import type { ServerSyncStageView } from "src/ts/storage/sync/serverSyncConnectFlow";

  let { stages }: { stages: ServerSyncStageView[] } = $props();
</script>

<ol class="mt-3 flex flex-col gap-2 text-sm">
  {#each stages as stage (stage.stage)}
    <li
      class="flex items-start gap-2"
      class:text-textcolor={stage.state === "active"}
      class:text-textcolor2={stage.state !== "active"}
      aria-current={stage.state === "active" ? "step" : undefined}
    >
      <span class="mt-0.5 shrink-0" aria-hidden="true">
        {#if stage.state === "done"}
          <Check size={16} />
        {:else if stage.state === "active"}
          <LoaderCircle size={16} class="motion-safe:animate-spin" />
        {:else}
          <Circle size={16} />
        {/if}
      </span>
      <span class="min-w-0 flex-1">{stage.label}</span>
      {#if stage.detail}<span class="text-textcolor2">{stage.detail}</span>{/if}
    </li>
  {/each}
</ol>
