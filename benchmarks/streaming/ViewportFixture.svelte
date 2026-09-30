<script lang="ts">
  import { onDestroy } from "svelte";
  import { writable } from "svelte/store";
  import Chats from "../../src/lib/ChatScreens/Chats.svelte";
  import type { character } from "../../src/ts/storage/database.svelte";
  import type { ChatViewportHandle } from "../../src/ts/chatViewport";
  import type { ConversationViewportSource } from "../../src/ts/conversationViewportSource";
  import type { LiveChatParserProjectionResolver } from "../../src/ts/selectedConversationLiveParserProjection";
  import { keepFocusedInputVisible } from "../../src/ts/gui/imeVisibility";
  import { restoreFocusAfterInputBlock } from "../../src/ts/ui/restoreFocusAfterInputBlock";

  let { currentCharacter, source, resolver }: {
    currentCharacter: character;
    source: ConversationViewportSource;
    resolver: LiveChatParserProjectionResolver;
  } = $props();
  let chats: ChatViewportHandle | undefined;
  const blocked = writable(false);
  onDestroy(restoreFocusAfterInputBlock(blocked));
  export function setInputBlocked(value: boolean) { blocked.set(value); }
  export function jumpTo(index: number) { return chats?.jumpTo(index); }
  export function settleStream() { currentCharacter.chats[0].isStreaming = false; }
</script>

<main data-viewport-fixture inert={$blocked} use:keepFocusedInputVisible={true}>
  <div data-viewport-scroll class="flex flex-col-reverse overflow-y-auto relative" style="height: 80vh; width: 100%;">
    <Chats bind:this={chats} {currentCharacter} viewportSource={source} parserProjectionResolver={resolver}
      onReroll={() => {}} unReroll={() => {}} currentUsername="Synthetic user" userIcon="" />
  </div>
</main>
