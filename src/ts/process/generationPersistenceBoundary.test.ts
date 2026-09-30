import 'fake-indexeddb/auto'
import { expect, it, vi } from 'vitest'
import type { Chat, Database } from '../storage/database.svelte'
import { IndexedDbPersistentDataStore } from '../storage/indexedDbPersistentDataStore'
import { createPersistentDataRuntime, capturePersistentRoot, capturePersistentPresets } from '../storage/persistentDataRuntime'
import { applyGenerationResponse } from './generationResponseApplication'
import { createMutationGatedPersistentDataStore } from '../storage/mutationGatedPersistentDataStore'
import { createStorageMutationGate } from '../storage/storageMutationGate'
import { prepareSummaryAwareGeneration } from './summaryAwareGenerationPreparation'

vi.mock('../storage/database.svelte', () => ({ presetTemplate: {} }))
vi.mock('../globalApi.svelte', () => ({ forageStorage: {} }))
vi.mock('src/ts/platform', () => ({ isTauri: false }))

it('streams through the real session and save coordinator and reopens the exact completed response', async () => {
    const name = `generation-reopen-${crypto.randomUUID()}`
    const store = new IndexedDbPersistentDataStore(name, indexedDB, IDBKeyRange)
    const database = { username: 'Synthetic', botPresets: [], characters: [{
        type: 'character', chaId: 'owner', name: 'Owner', chatPage: 0,
        chats: [{ id: 'chat', name: 'Chat', message: [{ role: 'user', data: 'Prompt', chatId: 'user' }] }],
    }] } as unknown as Database
    await store.open()
    await store.replaceFromDatabase(database)
    let current = structuredClone(database)
    const runtime = createPersistentDataRuntime({
        store, clock: { setTimeout: () => Symbol('owned-clock'), clearTimeout: () => {} },
        state: {
            captureRoot: () => capturePersistentRoot(current),
            capturePresets: () => capturePersistentPresets(current),
            captureSelectedCharacter: () => current.characters[0],
            captureCharacter: (id) => current.characters.find(character => character.chaId === id) ?? null,
            getSelectedCharacterId: () => current.characters[0].chaId,
            replaceDatabase: (replacement) => { current = replacement },
            publishCharacter: (character) => { current.characters[0] = character },
            publishConversation: (_owner, chat) => { current.characters[0].chats[0] = chat },
        },
        prepareDatabase: async candidate => structuredClone(candidate),
    })
    await runtime.initializeActiveWorkingSet(database)
    const chat = () => current.characters[0].chats[0]
    const response = await applyGenerationResponse({
        response: { type: 'streaming', result: new ReadableStream({
            start(controller) {
                controller.enqueue({ 0: 'First' })
                controller.enqueue({ 0: 'Final exact response' })
                controller.close()
            },
        }) },
        abortSignal: new AbortController().signal, continueGeneration: false,
        sayingCharacterId: 'owner', generationId: 'generated', generationInfo: {} as any, promptInfo: {} as any,
        removeIncompleteResponse: () => false, streamingDisplayOptimizationMode: () => 'off', ttsAutoSpeech: () => false,
        operation: {
            getCurrentSession: () => runtime.getActiveConversationSession(), getTargetChat: chat,
            isOwnerCurrent: () => true, publishTargetChat: (replacement) => { current.characters[0].chats[0] = replacement },
            invalidateSession: () => { throw new Error('Unexpected session replacement') }, incrementReloadKeys: () => {},
        },
        callbacks: {
            reformatContent: value => value, processOutput: async value => ({ data: value + ':processed', emoChanged: false }),
            runCurrentChatParser: value => value, runInlay: text => ({ text }), runOutputTrigger: async () => null,
            runOutputListeners: async () => {}, speak: async () => {}, trimIncompleteResponse: value => value,
            markResponseApplied: () => {}, onProviderFailure: message => { throw new Error(message) },
        },
    })
    expect(response).not.toBeNull()
    expect(response!.readOutput()?.data).toBe('Final exact response:processed')
    response!.release()
    await runtime.acknowledgeGenerationCompletion()
    const reopened = new IndexedDbPersistentDataStore(name, indexedDB, IDBKeyRange)
    await reopened.open()
    const recovered = await reopened.readConversation('owner', 'chat')
    expect(recovered?.value.message.map(message => [message.chatId, message.data])).toEqual([
        ['user', 'Prompt'], ['generated', 'Final exact response:processed'],
    ])
    expect((recovered?.value as Chat).isStreaming).not.toBe(true)
})


it('keeps bounded preparation available through the production mutation gate and selected authority', async () => {
    const raw = new IndexedDbPersistentDataStore(`bounded-production-${crypto.randomUUID()}`, indexedDB, IDBKeyRange)
    const store = createMutationGatedPersistentDataStore(raw, createStorageMutationGate())
    const messages = Array.from({ length: 430 }, (_, index) => ({
        role: index % 2 ? 'char' : 'user', data: `Synthetic body ${index}`, chatId: `m${index}`,
    }))
    const database = { username: 'Synthetic', botPresets: [], characters: [{
        type: 'character', chaId: 'owner', name: 'Owner', chatPage: 0,
        chats: [{ id: 'chat', name: 'Chat', message: messages, hypaV3Data: {
            summaries: [{ chatMemos: messages.slice(0, 300).map(message => message.chatId), summary: 'Synthetic summary' }],
        } }],
    }] } as unknown as Database
    await store.open()
    await store.replaceFromDatabase(database)
    let current = structuredClone(database)
    const runtime = createPersistentDataRuntime({
        store, clock: { setTimeout: () => Symbol('owned-clock'), clearTimeout: () => {} },
        state: {
            captureRoot: () => capturePersistentRoot(current),
            capturePresets: () => capturePersistentPresets(current),
            captureSelectedCharacter: () => current.characters[0],
            captureCharacter: id => current.characters.find(character => character.chaId === id) ?? null,
            getSelectedCharacterId: () => current.characters[0].chaId,
            getSelectedConversationId: () => current.characters[0].chats[0].id,
            replaceDatabase: replacement => { current = replacement },
            publishCharacter: character => { current.characters[0] = character },
            publishConversation: (_owner, chat) => { current.characters[0].chats[0] = chat },
            canUseWindowedSelectedConversation: () => true,
            captureActivationRollback: () => {
                const previous = structuredClone(current)
                return () => { current = previous }
            },
        },
        prepareDatabase: async candidate => structuredClone(candidate),
    })
    await runtime.initializeActiveWorkingSet(database)
    expect(await runtime.activateCharacter('owner')).toBe(true)
    const authority = runtime.captureSelectedConversationAuthority()
    expect(authority).not.toBeNull()
    if (!authority) throw new Error('Expected production windowed authority')
    const acquireComplete = vi.spyOn(runtime, 'acquireCompleteConversation')
    const prepared = await prepareSummaryAwareGeneration({
        store, authority, conversation: current.characters[0].chats[0],
        preserveOrphanedMemory: false, isCurrent: () => {
            const latest = runtime.captureSelectedConversationAuthority()
            return latest?.sessionToken === authority.sessionToken && latest.sessionVersion === authority.sessionVersion
        },
    })
    expect(prepared.route).toBe('summary-aware')
    if (prepared.route !== 'summary-aware') throw new Error(prepared.reason)
    expect(prepared.preparation.chat.message).toEqual(messages.slice(300))
    expect(prepared.preparation.plan.effectiveMessageMemos).toEqual(messages.map(message => message.chatId))
    expect(prepared.preparation.metrics).toMatchObject({ metadataRows: 430, bodyRows: 130, bodyPages: 3 })
    expect(runtime.captureSelectedConversationAuthority()?.sessionToken).toBe(authority.sessionToken)
    expect(acquireComplete).not.toHaveBeenCalled()
    await prepared.preparation.release()
})
