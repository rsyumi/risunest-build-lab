<script lang="ts">
  import { untrack } from 'svelte';
  import Chat from '../../src/lib/ChatScreens/Chat.svelte';
  import { DBState } from '../../src/ts/stores.svelte';
  import type { customscript } from '../../src/ts/storage/database.svelte';

  let { scripts, initial }: { scripts: customscript[], initial: string[] } = $props();
  let messages = $state(untrack(() => initial));
  const character = $derived({ ...DBState.db.characters[0], customscript: scripts, type: 'simple' as const });
  export function publish(index: number, text: string) { messages[index] = text; }
</script>

<section data-regex-probe>
  {#each messages as message, idx}
    <div data-regex-row={idx}>
      <Chat {message} {character} {idx} name="Synthetic" role="char" totalLength={messages.length} isLastMemory={false} />
    </div>
  {/each}
</section>
