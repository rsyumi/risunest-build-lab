import type { GenerationEndRecord, GenerationEndStatus } from '../process/generationEnd'
import { responseRange } from '../responseVariants'
import type { Message } from '../storage/database.svelte'
import { acquireCurrentRevisionWithRetry, withPersistentRevisionLease } from '../storage/persistentRecordIterator'
import { findCharacterIndex, findChatIndex } from './conversationContext'
import type { PinnedConversationPositionSource } from './pinnedConversationPosition'

export interface GenerationEndLocation {
    characterIndex: number
    chatIndex: number
    messageIndex: number
    messageId: string | null
}

export interface GenerationEndEvent extends GenerationEndLocation {
    status: GenerationEndStatus
    characterId: string
    conversationId: string
}

export interface GenerationEndListenerAccess {
    register(callback: (event: GenerationEndEvent) => unknown): { id: string }
    unregister(id: string): void
    dispose(): void
}

export interface GenerationEndEventDependencies {
    subscribe(listener: (record: GenerationEndRecord) => void): () => void
    locate(record: GenerationEndRecord): Promise<GenerationEndLocation>
    createId(): string
}

const unknownLocation: GenerationEndLocation = { characterIndex: -1, chatIndex: -1, messageIndex: -1, messageId: null }

/** The newest messages read to find the message a generation wrote. */
export const GENERATION_END_TAIL_MESSAGES = 64

/** The index in `messages` of the message the generation wrote, or -1. */
export function findGenerationEndMessage(messages: readonly Message[], record: GenerationEndRecord): number {
    const ids = new Set(record.messageIds)
    if (record.reroll) {
        // A reroll that did not complete restores the previous response.
        if (record.status !== 'completed') return -1
        // The projected carrier takes the variant group ID; its selected snapshot keeps the generated IDs.
        const range = responseRange(messages)
        const carrier = range ? messages[range.end - 1] : undefined
        const variants = carrier?.responseVariants
        const selected = variants?.candidates.find((candidate) => candidate.id === variants.selectedId)
        if (selected?.messages.some((message) => message.chatId !== undefined && ids.has(message.chatId))) {
            return range!.end - 1
        }
    }
    for (let index = messages.length - 1; index >= 0; index--) {
        const id = messages[index].chatId
        if (id !== undefined && ids.has(id)) return index
    }
    return -1
}

export function createGenerationEndLocator(source: () => PinnedConversationPositionSource) {
    let opening: Promise<void> | undefined
    return async (record: GenerationEndRecord): Promise<GenerationEndLocation> => {
        const { store, flushPendingData } = source()
        await flushPendingData('plugin-generation-end')
        await (opening ??= store.open().finally(() => {
            opening = undefined
        }))
        const lease = await acquireCurrentRevisionWithRetry(
            (revision) => store.acquireRevision(revision),
            async () => (await store.readRoot()).revision,
        )
        return withPersistentRevisionLease(lease, async (reader) => {
            const characterIndex = await findCharacterIndex(reader, record.characterId)
            if (characterIndex === null) return unknownLocation
            const chatIndex = await findChatIndex(reader, record.characterId, record.conversationId)
            if (chatIndex === null) return { ...unknownLocation, characterIndex }
            const window = await reader.readConversationWindow({
                characterId: record.characterId,
                conversationId: record.conversationId,
                limit: GENERATION_END_TAIL_MESSAGES,
            })
            const messages = window?.value.messages ?? []
            const index = findGenerationEndMessage(messages, record)
            if (index < 0) return { characterIndex, chatIndex, messageIndex: -1, messageId: null }
            return {
                characterIndex,
                chatIndex,
                messageIndex: window!.value.startIndex + index,
                messageId: messages[index].chatId ?? null,
            }
        })
    }
}

export function createGenerationEndEvents(dependencies: GenerationEndEventDependencies) {
    const listeners = new Map<string, { owner: string; callback: (event: GenerationEndEvent) => unknown }>()
    let stopListening: (() => void) | null = null
    let delivery: Promise<void> = Promise.resolve()

    function deliver(event: GenerationEndEvent): void {
        for (const [id, listener] of [...listeners]) {
            if (!listeners.has(id)) continue
            try {
                Promise.resolve(listener.callback({ ...event })).catch((error) => console.error(error))
            } catch (error) {
                console.error(error)
            }
        }
    }

    function receive(record: GenerationEndRecord): void {
        if (listeners.size === 0) return
        // Locations are read one after another, so events arrive in the order the generations ended.
        delivery = delivery.then(async () => {
            let location = unknownLocation
            try {
                location = await dependencies.locate(record)
            } catch (error) {
                console.error(error)
            }
            deliver({
                status: record.status,
                characterId: record.characterId,
                conversationId: record.conversationId,
                ...location,
            })
        })
    }

    function unregister(id: string, owner?: string): void {
        const listener = listeners.get(id)
        if (!listener || (owner !== undefined && listener.owner !== owner)) return
        listeners.delete(id)
        if (listeners.size === 0) {
            stopListening?.()
            stopListening = null
        }
    }

    return {
        forOwner(owner: string): GenerationEndListenerAccess {
            return {
                register(callback) {
                    const id = dependencies.createId()
                    listeners.set(id, { owner, callback })
                    stopListening ??= dependencies.subscribe(receive)
                    return { id }
                },
                unregister: (id) => unregister(id, owner),
                dispose() {
                    for (const [id, listener] of [...listeners]) {
                        if (listener.owner === owner) unregister(id)
                    }
                },
            }
        },
    }
}
