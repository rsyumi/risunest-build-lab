<script lang="ts">
    import { onMount, untrack, type Snippet } from 'svelte'
    import { get } from 'svelte/store'
    import { language } from 'src/lang'
    import { DBState, selectedCharID } from 'src/ts/stores.svelte'
    import { getPersistentDataRuntime } from 'src/ts/storage/persistentDataRuntime.svelte'
    import {
        SelectedConversationPromotionStaleError,
        type CompleteConversationLease,
        type SelectedConversationTarget,
    } from 'src/ts/storage/activeWorkingSet.svelte'
    import { navigationActivity } from 'src/ts/ui/navigationActivity'
    import { isMetadataOnlySelectedConversation } from 'src/ts/storage/selectedConversationLifecycle'
    import { isCatalogCharacterStub } from 'src/ts/storage/workingSetCatalog'
    import { isConversationSummaryStub } from 'src/ts/storage/conversationResidency'
    import { doingChat } from 'src/ts/process/generationState'
    import LoadingIndicator from '../UI/GUI/LoadingIndicator.svelte'

    let {
        children,
        active = true,
        close,
    }: { children: Snippet; active?: boolean; close?: () => void } = $props()
    let sourceVersion = $state(0)
    let ready = $state(false)
    let failed = $state(false)
    // Mounted children of the same character stay in place, inert, while the
    // next conversation is promoted, so a chat switch keeps their UI state.
    let retained = $state(false)
    let mountedCharacterId: string | undefined
    const runtime = getPersistentDataRuntime()
    let currentLease: CompleteConversationLease | undefined
    const STALE_PROMOTION_RETRIES = 2

    // Runs `next` once no navigation is in flight. Returns a cancel for a pending wait.
    function afterNavigation(next: () => void): () => void {
        if (!get(navigationActivity)) {
            next()
            return () => {}
        }
        let stop = () => {}
        stop = navigationActivity.subscribe((activity) => {
            if (activity) return
            stop()
            next()
        })
        return stop
    }

    // Saves may republish the same selection as new objects; only a change of
    // identity or residency needs a new lease.
    const selectionKey = $derived.by(() => {
        if (!active) return null
        const character = DBState.db.characters[$selectedCharID]
        if (!character) return null
        const conversation = character.chats[character.chatPage]
        return JSON.stringify([
            character.chaId,
            isCatalogCharacterStub(character),
            conversation?.id,
            !!conversation && isConversationSummaryStub(conversation),
            !!conversation && isMetadataOnlySelectedConversation(conversation),
        ])
    })
    const waitingForGeneration = $derived.by(() => {
        if (!active || !$doingChat) return false
        const character = DBState.db.characters[$selectedCharID]
        const conversation = character?.chats[character.chatPage]
        return !!conversation && isMetadataOnlySelectedConversation(conversation)
    })

    onMount(() =>
        runtime.subscribeActiveConversationViewportSource(() => {
            // Revision notifications do not change ownership. Retain the
            // mounted inputs (and focus) while autosaving the same session.
            if (
                currentLease &&
                runtime.getActiveConversationSession() === currentLease.session
            )
                return
            sourceVersion++
        }),
    )

    // Upstream editors bind directly to DBState, including mount-time defaults.
    // Mount them only after complete ownership has been adopted by the coordinator.
    $effect(() => {
        sourceVersion
        selectionKey
        const waiting = waitingForGeneration
        let disposed = false
        let lease: CompleteConversationLease | undefined
        let stopWaiting = () => {}
        untrack(() => {
            const character = active
                ? DBState.db.characters[$selectedCharID]
                : null
            const conversationId = character?.chats[character.chatPage]?.id
            retained =
                (ready || retained) &&
                !!character &&
                !isCatalogCharacterStub(character) &&
                character.chaId === mountedCharacterId
            ready = false
            failed = false
            if (!character || waiting) return
            const settle = (complete: boolean) => {
                ready = complete
                failed = !complete
                retained = false
                if (complete) mountedCharacterId = character.chaId
            }
            const target = runtime.captureSelectedConversationTarget()
            if (!target) {
                // Nonpersistent playgrounds and complete upstream working sets
                // have no selected session. Never expose a partial shell here.
                const conversation = character.chats[character.chatPage]
                settle(
                    !isCatalogCharacterStub(character) &&
                        !!conversation &&
                        !isConversationSummaryStub(conversation) &&
                        !isMetadataOnlySelectedConversation(conversation),
                )
                return
            }
            if (
                target.characterId !== character.chaId ||
                target.conversationId !== conversationId
            ) {
                settle(false)
                return
            }
            const fail = (error: unknown) => {
                console.error('Bound editor promotion failed', error)
                settle(false)
            }
            const acquire = (
                selection: SelectedConversationTarget,
                retries: number,
            ) => {
                void runtime
                    .acquireCompleteConversation('bound-editor', selection)
                    .then((acquired) => {
                        if (disposed) {
                            acquired.release()
                            return
                        }
                        lease = acquired
                        currentLease = acquired
                        settle(true)
                    })
                    .catch((error) => {
                        if (disposed) return
                        if (!(error instanceof SelectedConversationPromotionStaleError)) {
                            fail(error)
                            return
                        }
                        // A navigation fences the promotion long before it publishes the
                        // next selection. A moved selection replaces this run, and the
                        // same selection retries a few times.
                        stopWaiting = afterNavigation(() => {
                            if (disposed) return
                            const current = runtime.captureSelectedConversationTarget()
                            if (
                                current?.characterId !== character.chaId ||
                                current.conversationId !== conversationId
                            )
                                sourceVersion++
                            else if (retries > 0) acquire(current, retries - 1)
                            else fail(error)
                        })
                    })
            }
            acquire(target, STALE_PROMOTION_RETRIES)
        })
        return () => {
            disposed = true
            stopWaiting()
            if (currentLease === lease) currentLease = undefined
            lease?.release()
        }
    })
</script>

{#if active}
    {#if ready || retained}
        <div class="contents" inert={!ready} aria-busy={!ready}>
            {@render children()}
        </div>
    {:else}
        <!-- Overlay mounts pass `close` and get no backdrop from the parent. -->
        <div
            class={close
                ? 'absolute inset-0 flex flex-col items-center justify-center gap-3 bg-black/50 p-4 text-textcolor'
                : 'flex flex-col items-start gap-3 p-2 text-textcolor'}
        >
            {#if failed}
                <div
                    class="flex flex-col items-center gap-3 text-center"
                    role="alert"
                >
                    <span>{language.error}</span>
                    <button
                        class="rounded-lg border border-borderc px-3 py-1.5 hover:bg-darkbg"
                        onclick={() => sourceVersion++}
                        >{language.hypaV3Modal.retry}</button
                    >
                </div>
            {:else if waitingForGeneration}
                <span>{language.navigationBlockedWhileGenerating}</span>
            {:else}
                <LoadingIndicator label={language.loadingChatData} compact />
            {/if}
            {#if close}
                <button
                    class="rounded-lg border border-borderc px-3 py-1.5 hover:bg-darkbg"
                    onclick={close}>{language.cancel}</button
                >
            {/if}
        </div>
    {/if}
{/if}
