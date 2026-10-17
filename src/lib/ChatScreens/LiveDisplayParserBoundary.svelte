<script lang="ts">
    import { onMount, untrack, type Snippet } from 'svelte'
    import {
        acquireLiveDisplayParserLease,
        captureLiveDisplayParserInputs,
        captureLiveDisplayParserSelection,
        subscribeLiveDisplayParserSelection,
    } from 'src/ts/liveDisplayParserLease'
    import {
        getCurrentCharacter,
        getCurrentChat,
    } from 'src/ts/storage/database.svelte'
    import { language } from 'src/lang'
    import LoadingIndicator from '../UI/GUI/LoadingIndicator.svelte'
    import type { BoundedLiveChatParserProjection } from 'src/ts/selectedConversationLiveParserProjection'

    let {
        source,
        character,
        children,
        refreshRevision = 0,
        preservePendingContent = false,
        chatID = -1,
        parseFirst = false,
    }: {
        source: unknown
        character: Parameters<typeof captureLiveDisplayParserInputs>[1]
        children: Snippet<[AbortSignal, BoundedLiveChatParserProjection | undefined]>
        chatID?: number
        parseFirst?: boolean
        refreshRevision?: number
        preservePendingContent?: boolean
    } = $props()
    let readySignal = $state<AbortSignal | null>(null)
    let projection = $state.raw<BoundedLiveChatParserProjection | undefined>()
    let historyRevision = $state('')
    let failed = $state(false)
    let retry = $state(0)
    let activeController: AbortController | null = null
    let readyOwner: string | null = null
    let preparing = false
    let selectionRevision = 0
    let selectionIdentity = $state(untrack(captureLiveDisplayParserSelection))
    onMount(() =>
        subscribeLiveDisplayParserSelection((identity, revision) => {
            selectionRevision++
            if (preparing && identity === selectionIdentity) return
            historyRevision = revision ?? ''
            if (identity !== selectionIdentity) activeController?.abort()
            else if (failed) retry++
            selectionIdentity = identity
        }, () => projection?.context.parserContext.historyAccess?.dependencies),
    )
    const signature = $derived(
        JSON.stringify({
            characterId: getCurrentCharacter()?.chaId,
            conversationId: getCurrentChat()?.id,
            chatID,
            parseFirst,
            inputs: captureLiveDisplayParserInputs(source, character),
        }),
    )
    const owner = $derived(
        JSON.stringify([
            getCurrentCharacter()?.chaId,
            getCurrentChat()?.id,
            selectionIdentity,
        ]),
    )

    $effect(() => {
        void signature
        void selectionIdentity
        void retry
        void refreshRevision
        void historyRevision
        const requestedOwner = owner
        const requestedRevision = selectionRevision
        const controller = new AbortController()
        preparing = true
        activeController = controller
        let lease: { release(): void } | null = null
        // Retain the published DOM while a refresh is admitted, but never carry
        // another conversation's markup or styles across navigation.
        if (!preservePendingContent || readyOwner !== requestedOwner)
            readySignal = null
        failed = false
        void untrack(() =>
            acquireLiveDisplayParserLease({
                source,
                character,
                signal: controller.signal,
                chatID,
                parseFirst,
                // Abort first so the stale render is discarded rather than shown as failed.
                onStale: () => {
                    controller.abort()
                    retry++
                },
            }),
        )
            .then((acquired) => {
                if (controller.signal.aborted) {
                    acquired?.release()
                    return
                }
                lease = acquired
                preparing = false
                projection = acquired?.projection
                readyOwner = requestedOwner
                readySignal = controller.signal
            })
            .catch(() => {
                if (controller.signal.aborted) return
                preparing = false
                // Preparation may complete elsewhere before this rejection arrives.
                if (selectionRevision !== requestedRevision) retry++
                else failed = true
            })
        return () => {
            controller.abort()
            if (activeController === controller) activeController = null
            lease?.release()
        }
    })

</script>

{#if readySignal}
    {@render children(readySignal, projection)}
{/if}
{#if failed}
    <div
        class="flex flex-col items-center gap-3 p-3 text-center"
        role="alert"
        data-live-display-load-error
    >
        <span>{language.chatDataLoadFailed}</span>
        <button
            class="rounded-lg border border-borderc px-3 py-1.5 hover:bg-darkbg"
            onclick={() => (retry += 1)}
        >
            {language.hypaV3Modal.retry}
        </button>
    </div>
{:else if !readySignal}
    <!-- Both mount points are inline: a background layer and an expanded bookmark row. -->
    <LoadingIndicator label={language.loadingChatData} compact />
{/if}
