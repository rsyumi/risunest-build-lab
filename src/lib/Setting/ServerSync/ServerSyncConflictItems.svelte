<script lang="ts">
  import { language } from "src/lang";
  import type { ServerCycle } from "src/ts/storage/sync/serverSync";
  import { summarizeServerSyncConflict, type ServerConflictSummary } from "src/ts/storage/sync/serverSyncConflictSummary";
  let { preview }: { preview: ServerCycle } = $props();
  let summary = $state<ServerConflictSummary>();
  $effect(() => {
    const current = preview;
    let active = true;
    summary = undefined;
    void (async () => {
      const fallback = await summarizeServerSyncConflict(current);
      if (active) summary = fallback;
      try {
        const { getPersistentDataRuntime } = await import("src/ts/storage/persistentDataRuntime.svelte");
        if (!active) return;
        const lease = await getPersistentDataRuntime().store.acquireRevision(current.localRevision);
        try {
          const resolved = await summarizeServerSyncConflict(current, lease);
          if (active) summary = resolved;
        } finally { await lease.release(); }
      } catch { /* Identifiers remain available if the preview revision expired. */ }
    })();
    return () => { active = false; };
  });
</script>

{#if summary}
  <ul class="my-2 text-sm text-textcolor2 break-all">
    {#each summary.groups as group}
      <li>{language.risuNest.serverSync.conflictKinds[group.kind]}: {group.names.filter(Boolean).join(", ")}{group.count > group.names.length ? ` +${group.count - group.names.length}` : ""}</li>
    {/each}
    {#if summary.remaining > 0}<li>+{summary.remaining}</li>{/if}
  </ul>
{/if}
