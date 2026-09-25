import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import type { Chat, Database, Message, character } from '../database.svelte'
import { IndexedDbPersistentDataStore } from '../indexedDbPersistentDataStore'
import { capturePersistentRoot, createPersistentDataRuntime,
    publishPersistentConversationReplacementToWorkingSet,
    type PersistentDataRuntimeStateAdapter } from '../persistentDataRuntime'

export const VIEWPORT_ROW_BUDGET = 64

function makeMessage(index: number, duplicatePositions: readonly number[]): Message {
    return {
        role: index % 2 === 0 ? 'user' : 'char',
        data: `turn-${index.toString().padStart(5, '0')}`,
        chatId: duplicatePositions.includes(index) ? 'duplicate-anchor' : `message-${index}`,
        name: index % 97 === 0 ? `speaker-${index}` : undefined,
        saying: index % 131 === 0 ? `aside-${index}` : undefined,
    }
}

export function makeConversation(messageCount = 10_000, duplicatePositions: readonly number[] = [100, 9_000]): Chat {
    return {
        id: 'chat-a',
        name: 'Corpus conversation',
        note: 'synthetic eviction owner',
        localLore: [],
        fmIndex: -1,
        message: Array.from({ length: messageCount }, (_, index) => makeMessage(index, duplicatePositions)),
    }
}

export function makeDatabase(conversation: Chat): Database {
    return {
        username: 'Eviction corpus',
        botPresets: [],
        botPresetsId: 0,
        pluginCustomStorage: {},
        statics: { messages: 0 },
        aiModel: 'test-model',
        maxContext: 8_192,
        maxResponse: 128,
        promptTemplate: [{ type: 'chat', rangeStart: 0, rangeEnd: 'end' }],
        promptSettings: { trimStartNewChat: true, sendName: false },
        customPromptTemplateToggle: '', globalChatVariables: {}, mainPrompt: '',
        additionalPrompt: '', globalNote: '', jailbreak: '', jailbreakToggle: false,
        chainOfThought: false, personaPrompt: false, promptPreprocess: false,
        descriptionPrefix: '', formatingOrder: [], bias: [], outputImageModal: false,
        rememberToolUsage: false, streamingDisplayOptimizationMode: 'off',
        autoContinueMinTokens: 0, autoContinueChat: false, notification: false,
        ttsAutoSpeech: false, supaModelType: 'none', hanuraiEnable: false,
        hypav2: false, hypaV3: false,
        characters: [{
            type: 'character',
            chaId: 'char-a',
            name: 'Synthetic owner',
            firstMessage: 'Greeting',
            alternateGreetings: [],
            desc: '', personality: '', scenario: '', bias: [], additionalAssets: [],
            emotionImages: [], reloadKeys: 0, viewScreen: 'none', inlayViewScreen: false,
            supaMemory: false, utilityBot: false,
            chatPage: 0,
            chats: [conversation],
        }],
    } as unknown as Database
}

export async function createEvictionFixture(initialConversation = makeConversation()) {
    let workingCopy = structuredClone(makeDatabase(initialConversation))
    workingCopy.characters.push({
        type: 'character',
        chaId: 'char-b',
        name: 'Non-target owner',
        chatPage: 0,
        chats: [{
            id: 'chat-b',
            name: 'Non-target conversation',
            note: '',
            localLore: [],
            message: [{ role: 'user', data: 'must remain unread' }],
        }],
    } as character)
    const store = new IndexedDbPersistentDataStore(
        `selected-eviction-corpus-${crypto.randomUUID()}`,
        new IDBFactory(),
        IDBKeyRange,
    )
    await store.open()
    const initial = await store.replaceFromDatabase(workingCopy)
    const selectedConversation = () => {
        const owner = workingCopy.characters[0]
        return owner.chats[owner.chatPage ?? 0]
    }
    const state: PersistentDataRuntimeStateAdapter = {
        captureRoot: () => capturePersistentRoot(workingCopy),
        captureSelectedCharacter: () => workingCopy.characters[0] ?? null,
        captureCharacter: (id) =>
            workingCopy.characters.find((candidate) => candidate.chaId === id) ?? null,
        getSelectedCharacterId: () => workingCopy.characters[0]?.chaId,
        getSelectedConversationId: () => selectedConversation()?.id,
        replaceDatabase: (next) => {
            workingCopy = next
        },
        publishCharacter: (next) => {
            workingCopy.characters[0] = next
        },
        publishConversation: (_characterId, conversation, nextCharacter) => {
            if (nextCharacter) workingCopy.characters[0] = nextCharacter
            else workingCopy.characters[0].chats[workingCopy.characters[0].chatPage ?? 0] = conversation
        },
        publishConversationReplacement: (result) => {
            publishPersistentConversationReplacementToWorkingSet(workingCopy, result)
        },
        canUseWindowedSelectedConversation: () => true,
        isConversationOperationActive: () => false,
        conversationViewportRowBudget: VIEWPORT_ROW_BUDGET,
    }
    const backgroundErrors: unknown[] = []
    const runtime = createPersistentDataRuntime({
        store,
        state,
        onBackgroundError: (error) => { backgroundErrors.push(error) },
        prepareDatabase: async (candidate) => candidate,
    })
    return {
        get workingCopy() { return workingCopy },
        set workingCopy(value: Database) { workingCopy = value },
        store, initial, state, runtime, selectedConversation, backgroundErrors,
    }
}
