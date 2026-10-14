import { describe, expect, it, vi } from 'vitest'
import type { Chat, Database, character } from './database.svelte'
import type { WorkingSetCommit } from './persistentDataStore'
import { createStoreStub } from './persistentDataStore.testSupport'
import type { PersistentConversationReplacementResult } from './saveCoordinator'
import {
    captureRoot,
    SaveCoordinator,
} from './saveCoordinator.testSupport'

const CONVERSATIONS_PER_CHARACTER = 10
const MESSAGES_PER_CONVERSATION = 20
const WARM_RUNS = 5
const MEASURED_RUNS = 5

function makeConversation(characterIndex: number, conversationIndex: number): Chat {
    return {
        id: `chat-${characterIndex}-${conversationIndex}`,
        name: `Chat ${characterIndex}-${conversationIndex}`,
        message: Array.from({ length: MESSAGES_PER_CONVERSATION }, (_, messageIndex) => ({
            role: messageIndex % 2 === 0 ? 'user' : 'char',
            data: `Synthetic message ${characterIndex}-${conversationIndex}-${messageIndex}`,
        })),
    } as Chat
}

function makeSyntheticLibrary(characterCount: number): {
    database: Database
    unrelatedCharacterClonePropertyReads: () => number
    unrelatedConversationClonePropertyReads: () => number
    resetUnrelatedClonePropertyReads: () => void
} {
    let unrelatedCharacterReads = 0
    let unrelatedConversationReads = 0
    const targetCharacterIndex = characterCount - 1
    const characters = Array.from({ length: characterCount }, (_, characterIndex) => {
        const chats = Array.from(
            { length: CONVERSATIONS_PER_CHARACTER },
            (_, conversationIndex) => makeConversation(characterIndex, conversationIndex),
        )
        const item = {
            type: 'character',
            chaId: `char-${characterIndex}`,
            name: `Character ${characterIndex}`,
            chats,
        } as character
        if (characterIndex !== targetCharacterIndex) {
            Object.defineProperty(item, 'syntheticCloneTrap', {
                enumerable: true,
                configurable: true,
                get() {
                    unrelatedCharacterReads++
                    return characterIndex
                },
            })
            for (const [conversationIndex, conversation] of chats.entries()) {
                Object.defineProperty(conversation, 'syntheticCloneTrap', {
                    enumerable: true,
                    configurable: true,
                    get() {
                        unrelatedConversationReads++
                        return conversationIndex
                    },
                })
            }
        }
        return item
    })
    return {
        database: {
            username: 'Scoped scale fixture',
            botPresets: [],
            characters,
        } as unknown as Database,
        unrelatedCharacterClonePropertyReads: () => unrelatedCharacterReads,
        unrelatedConversationClonePropertyReads: () => unrelatedConversationReads,
        resetUnrelatedClonePropertyReads: () => {
            unrelatedCharacterReads = 0
            unrelatedConversationReads = 0
        },
    }
}

async function expectScopedConversationWrites(characterCount: number) {
    const fixture = makeSyntheticLibrary(characterCount)
    const { database } = fixture
    const targetCharacter = database.characters[characterCount - 1]
    const targetCharacterId = targetCharacter.chaId
    const targetConversationId = targetCharacter.chats[0].id
    let revision = 1
    let durableConversation = structuredClone(targetCharacter.chats[0])
    const targetReads: Array<[string, string]> = []
    const commit = vi.fn(async (input: WorkingSetCommit) => {
        expect(input.conversations).toBeUndefined()
        expect(input.unitMutations).toHaveLength(1)
        for (const mutation of input.unitMutations ?? []) {
            const key = JSON.parse(mutation.key)
            expect(key).toEqual(['conversation', targetCharacterId, targetConversationId, 'name'])
            expect(mutation.type).toBe('set')
            if (mutation.type === 'delete') delete (durableConversation as unknown as Record<string, unknown>)[key[3]]
            else Object.defineProperty(durableConversation, key[3], {value: structuredClone(mutation.value), enumerable: true, configurable: true, writable: true})
        }
        return { revision: ++revision }
    })
    const store = createStoreStub({
        commit,
        readConversation: vi.fn(async (characterId: string, conversationId: string) => {
            targetReads.push([characterId, conversationId])
            if (characterId !== targetCharacterId || conversationId !== targetConversationId) {
                throw new Error(`Unexpected unrelated conversation read: ${characterId}/${conversationId}`)
            }
            return { revision, value: structuredClone(durableConversation) }
        }),
        readCharacter: vi.fn(async () => {
            throw new Error('Scoped conversation write must not hydrate a character')
        }),
        queryConversations: vi.fn(async () => {
            throw new Error('Scoped conversation write must not scan conversations')
        }),
        materializeDatabase: vi.fn(async () => {
            throw new Error('Scoped conversation write must not materialize the database')
        }),
    })
    const coordinator = new SaveCoordinator({
        store,
        captureRoot: () => captureRoot(database),
        captureSelectedCharacter: () => targetCharacter,
        captureCharacter: (id) =>
            database.characters.find((item) => item.chaId === id) ?? null,
        replaceDatabase: () => undefined,
        publishConversationReplacement: (result: PersistentConversationReplacementResult) => {
            targetCharacter.chats[0] = structuredClone(result.conversation)
        },
    })
    coordinator.initialize(revision, database)
    fixture.resetUnrelatedClonePropertyReads()

    const run = async (sample: number) => {
        const replacement = {
            ...structuredClone(targetCharacter.chats[0]),
            name: `Scoped edit ${sample}`,
        } as Chat
        await expect(coordinator.replacePersistentConversation(
            targetCharacterId,
            targetConversationId,
            'scoped-scale-measurement',
            replacement,
        )).resolves.toBe(true)
    }

    for (let sample = 0; sample < WARM_RUNS; sample++) await run(sample)
    commit.mockClear()
    targetReads.length = 0
    fixture.resetUnrelatedClonePropertyReads()

    for (let sample = 0; sample < MEASURED_RUNS; sample++) await run(WARM_RUNS + sample)

    expect(commit).toHaveBeenCalledTimes(MEASURED_RUNS)
    expect(targetReads).toEqual(Array.from(
        { length: MEASURED_RUNS },
        () => [targetCharacterId, targetConversationId],
    ))
    expect(store.readCharacter).not.toHaveBeenCalled()
    expect(store.queryConversations).not.toHaveBeenCalled()
    expect(store.materializeDatabase).not.toHaveBeenCalled()
    expect(fixture.unrelatedCharacterClonePropertyReads()).toBe(0)
    expect(fixture.unrelatedConversationClonePropertyReads()).toBe(0)
}

describe('SaveCoordinator scoped conversation scale', () => {
    it('keeps an edit in a 100 character library bounded to one target', async () => {
        await expectScopedConversationWrites(100)
    })
})
