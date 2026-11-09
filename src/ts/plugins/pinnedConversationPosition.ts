import type { PersistentDataStore } from '../storage/persistentDataStore'
import { getPersistentDataStore } from '../storage/persistentDataStoreFactory'
import { acquireCurrentRevisionWithRetry, withPersistentRevisionLease } from '../storage/persistentRecordIterator'
import { findCharacterIndex, findChatIndex } from './conversationContext'

/** Where the index APIs find a conversation; -1 where they find nothing. */
export interface PinnedConversationPosition {
    characterIndex: number
    chatIndex: number
}

export interface PinnedConversationPositionSource {
    store: Pick<PersistentDataStore, 'open' | 'acquireRevision' | 'readRoot'>
    flushPendingData(reason: string): Promise<void>
}

export function createPinnedConversationPositionResolver(source: () => PinnedConversationPositionSource) {
    let opening: Promise<void> | undefined
    return async (characterId: string, conversationId: string | null): Promise<PinnedConversationPosition> => {
        const { store, flushPendingData } = source()
        // The index APIs flush before they read, so the position is taken from the same state.
        await flushPendingData('plugin-conversation-position')
        await (opening ??= store.open().finally(() => {
            opening = undefined
        }))
        const lease = await acquireCurrentRevisionWithRetry(
            (revision) => store.acquireRevision(revision),
            async () => (await store.readRoot()).revision,
        )
        return withPersistentRevisionLease(lease, async (reader) => {
            const characterIndex = await findCharacterIndex(reader, characterId)
            if (characterIndex === null) return { characterIndex: -1, chatIndex: -1 }
            const chatIndex = conversationId === null ? null : await findChatIndex(reader, characterId, conversationId)
            return { characterIndex, chatIndex: chatIndex ?? -1 }
        })
    }
}

export const resolvePinnedConversationPosition = createPinnedConversationPositionResolver(() => ({
    store: getPersistentDataStore(),
    flushPendingData: async (reason) => {
        const { flushPendingDataLocally } = await import('../storage/persistentDataRuntime.svelte')
        await flushPendingDataLocally(reason)
    },
}))
