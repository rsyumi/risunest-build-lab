import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'

import { ActiveConversationSession } from '../../storage/activeConversationSession'
import { doingChat as generationDoingChat } from '../../process/generationState'
import {
    noteGenerationMessage,
    noteGenerationStarted,
    subscribeGenerationEnd,
    type GenerationEndRecord,
} from '../../process/generationEnd'

const mocks = vi.hoisted(() => ({
    authoritativeLoad: vi.fn(async()=>{}),
    api: null as any,
    database: null as any,
    selectedId: 0,
    authorityEpoch: 0,
    session: null as any,
    selectedTarget: null as any,
    acquireCompleteConversation: vi.fn(),
    processSendChat: vi.fn(),
    doingChat: false,
}))

vi.mock('../plugins.svelte', async () => (await import('./v3Host.testSupport')).pluginsModule({
    loadPluginsAfterAuthoritativeRestore: mocks.authoritativeLoad,
    pluginV2: { providers: new Map(), providerOptions: new Map() },
}))
vi.mock('./factory', async () => (await import('./v3Host.testSupport')).sandboxHostModule((api) => { mocks.api = api }))
vi.mock('src/ts/storage/database.svelte', async () => (await import('./v3Host.testSupport')).databaseModule({
    read: () => mocks.database,
    write: (database) => { mocks.database = database },
    selectedIndex: () => mocks.selectedId,
}))
vi.mock('../pluginSafeClass', async () => (await import('./v3Host.testSupport')).pluginSafeClassModule())
vi.mock('src/ts/stores.svelte', async () => (await import('./v3Host.testSupport')).storesModule({
    read: () => mocks.database,
    write: (database) => { mocks.database = database },
    selectedIndex: () => mocks.selectedId,
}))
vi.mock('src/ts/alert', async () => (await import('./v3Host.testSupport')).alertModule())
vi.mock('src/ts/util', async () => (await import('./v3Host.testSupport')).utilModule())
vi.mock('src/ts/globalApi.svelte', () => ({ checkCharOrder: vi.fn(), forageStorage: {}, getFetchLogs: vi.fn() }))
vi.mock('src/ts/gui/colorscheme', async () => (await import('./v3Host.testSupport')).colorschemeModule())
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('src/ts/process/mcp/pluginmcp', async () => (await import('./v3Host.testSupport')).pluginMcpModule())
vi.mock('src/ts/process/files/inlays', async () => (await import('./v3Host.testSupport')).inlaysModule())
vi.mock('src/ts/translator/translator', async () => (await import('./v3Host.testSupport')).translatorModule())
vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule())
vi.mock('localforage', async () => (await import('../localforage.testSupport')).localforageModule())
vi.mock('src/ts/process/index.svelte', async () => (await import('./v3Host.testSupport')).processIndexModule({
    sendChat: mocks.processSendChat,
    doingChat: {
        subscribe(run: (value: boolean) => void) { run(mocks.doingChat); return () => undefined },
        set(value: boolean) { mocks.doingChat = value },
    },
}))
vi.mock('src/ts/model/modellist', async () => (await import('./v3Host.testSupport')).modellistModule())
vi.mock('src/ts/process/request/request', async () => (await import('./v3Host.testSupport')).requestModule())
vi.mock('src/ts/process/modules', async () => (await import('./v3Host.testSupport')).modulesModule())
vi.mock('src/ts/process/ttsHooks', async () => (await import('./v3Host.testSupport')).ttsHooksModule())
vi.mock('src/ts/storage/persistentDataRuntime.svelte', async () => (await import('./v3Host.testSupport')).persistentRuntimeModule({
    getPersistentRevision: () => 1,
    commitPersistentUnitIntent: vi.fn(async () => {}),
    acquireCompleteConversation: mocks.acquireCompleteConversation,
    captureSelectedConversationTarget: () => mocks.selectedTarget,
    assertPersistentMutationAllowed: (epoch = mocks.authorityEpoch) => {
        if (epoch !== mocks.authorityEpoch) throw new Error('Persistent mutation fenced')
    },
    getPersistentStorageAuthorityEpoch: () => mocks.authorityEpoch,
    getActiveConversationSession: () => mocks.session,
    getPersistentDataRuntime: vi.fn(),
}))
vi.mock('../pluginDatabaseAccess', async () => (await import('./v3Host.testSupport')).pluginDatabaseAccessModule())

import { executePluginV3 } from './v3.svelte'

function deferred<T>() {
    let resolve!: (value: T) => void
    const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise })
    return { promise, resolve }
}

describe('Plugin v3 sendChat complete mutation gateway', () => {
    beforeEach(async () => {
        vi.clearAllMocks()
        generationDoingChat.set(false)
        mocks.api = null
        mocks.selectedId = 0
        mocks.doingChat = false
        const chat = { id: 'chat-a', message: [{ role: 'char', data: 'before' }] }
        const character = { type: 'character', chaId: 'character-a', chatPage: 0, chats: [chat] }
        mocks.database = { aiModel: 'test-model', plugins: [{ name: 'lease-plugin', script: '' }], characters: [character] }
        mocks.session = new ActiveConversationSession({
            characterId: character.chaId,
            conversationId: chat.id,
            conversation: chat as any,
            storeRevision: 1,
        })
        mocks.selectedTarget = null
        mocks.acquireCompleteConversation.mockReset()
        mocks.processSendChat.mockReset()
        await executePluginV3({ name: `lease-plugin-${crypto.randomUUID()}`, script: '' } as any)
    })

    it('preserves direct no-session send behavior when no selected target exists', async () => {
        const chat = mocks.database.characters[0].chats[0]
        mocks.processSendChat.mockResolvedValue(true)

        await expect(mocks.api.sendChat('hello')).resolves.toBe(true)

        expect(chat.message.map((message: any) => message.data)).toEqual(['before', 'hello'])
        expect(mocks.acquireCompleteConversation).not.toHaveBeenCalled()
        expect(mocks.processSendChat).toHaveBeenCalledOnce()
    })

    it('shares the public reservation before async promotion so concurrent calls cannot both append', async () => {
        const chat = mocks.database.characters[0].chats[0]
        const target = { characterId: 'character-a', conversationId: 'chat-a' }
        mocks.selectedTarget = target
        const firstPromotion = deferred<any>()
        const secondReachedPromotion = new Error('second call reached promotion')
        mocks.acquireCompleteConversation
            .mockReturnValueOnce(firstPromotion.promise)
            .mockRejectedValueOnce(secondReachedPromotion)
        mocks.processSendChat.mockResolvedValue(true)
        let firstReleases = 0

        const first = mocks.api.sendChat('first')
        while (mocks.acquireCompleteConversation.mock.calls.length === 0) await Promise.resolve()
        const secondOutcome = await mocks.api.sendChat('second').catch((error: unknown) => error)

        expect(get(generationDoingChat)).toBe(true)

        firstPromotion.resolve({
            session: mocks.session,
            target,
            release() { firstReleases += 1 },
        })
        await expect(first).resolves.toBe(true)

        expect(secondOutcome).toBeInstanceOf(Error)
        expect((secondOutcome as Error).message).toBe('A chat is already in progress')
        expect(mocks.acquireCompleteConversation).toHaveBeenCalledTimes(1)
        expect(chat.message.map((message: any) => message.data)).toEqual(['before', 'first'])
        expect(firstReleases).toBe(1)
    })

    it('returns false and removes only its appended message when generation declines', async () => {
        const chat = mocks.database.characters[0].chats[0]
        mocks.processSendChat.mockResolvedValue(false)

        await expect(mocks.api.sendChat('declined')).resolves.toBe(false)

        expect(chat.message.map((message: any) => message.data)).toEqual(['before'])
    })

    it('removes only its appended message and releases once when generation throws', async () => {
        const chat = mocks.database.characters[0].chats[0]
        const target = { characterId: 'character-a', conversationId: 'chat-a' }
        mocks.selectedTarget = target
        const failure = new Error('generation failed')
        mocks.processSendChat.mockRejectedValue(failure)
        let releaseCount = 0
        mocks.acquireCompleteConversation.mockResolvedValue({
            session: mocks.session,
            target,
            release() { releaseCount += 1 },
        })

        await expect(mocks.api.sendChat('failed')).rejects.toBe(failure)

        expect(chat.message.map((message: any) => message.data)).toEqual(['before'])
        expect(releaseCount).toBe(1)
    })

    it('promotes before append and holds one exact lease until delegated generation settles', async () => {
        const chat = mocks.database.characters[0].chats[0]
        const target = { characterId: 'character-a', conversationId: 'chat-a' }
        mocks.selectedTarget = target
        const promotion = deferred<any>()
        mocks.acquireCompleteConversation.mockReturnValue(promotion.promise)
        const generation = deferred<boolean>()
        mocks.processSendChat.mockReturnValue(generation.promise)
        let releaseCount = 0

        const sending = mocks.api.sendChat('hello')
        while (mocks.acquireCompleteConversation.mock.calls.length === 0) await Promise.resolve()

        expect(mocks.acquireCompleteConversation).toHaveBeenCalledOnce()
        expect(chat.message.map((message: any) => message.data)).toEqual(['before'])
        expect(mocks.processSendChat).not.toHaveBeenCalled()

        const pin = mocks.session.acquirePin('compatibility')
        promotion.resolve({
            session: mocks.session,
            target,
            release() {
                releaseCount += 1
                pin.release()
            },
        })
        while (mocks.processSendChat.mock.calls.length === 0) await Promise.resolve()

        expect(chat.message.map((message: any) => message.data)).toEqual(['before', 'hello'])
        expect(mocks.session.pinCount('compatibility')).toBe(1)

        generation.resolve(true)
        await expect(sending).resolves.toBe(true)
        expect(releaseCount).toBe(1)
        expect(mocks.session.pinCount('compatibility')).toBe(0)
    })

    it('aborts before append when the promoted lease does not own the selected chat', async () => {
        const chat = mocks.database.characters[0].chats[0]
        const otherChat = { id: 'chat-b', message: [] }
        const otherSession = new ActiveConversationSession({
            characterId: 'character-a',
            conversationId: 'chat-b',
            conversation: otherChat as any,
            storeRevision: 1,
        })
        mocks.selectedTarget = { characterId: 'character-a', conversationId: 'chat-a' }
        let releaseCount = 0
        mocks.acquireCompleteConversation.mockResolvedValue({
            session: otherSession,
            target: mocks.selectedTarget,
            release() { releaseCount += 1 },
        })

        await expect(mocks.api.sendChat('hello')).resolves.toBe(false)

        expect(chat.message.map((message: any) => message.data)).toEqual(['before'])
        expect(mocks.processSendChat).not.toHaveBeenCalled()
        expect(releaseCount).toBe(1)
    })
})

describe('Plugin v3 sendChat generation end event', () => {
    const target = { characterId: 'character-a', conversationId: 'chat-a' }
    const stopListening: Array<() => void> = []
    let releaseCount = 0

    function listenGenerationEnd() {
        const records: Array<GenerationEndRecord & { busy: boolean, released: number }> = []
        stopListening.push(subscribeGenerationEnd((record) => records.push({
            ...record,
            busy: get(generationDoingChat),
            released: releaseCount,
        })))
        return records
    }

    beforeEach(async () => {
        vi.clearAllMocks()
        generationDoingChat.set(false)
        mocks.api = null
        mocks.selectedId = 0
        mocks.doingChat = false
        releaseCount = 0
        const chat = { id: 'chat-a', message: [{ role: 'char', data: 'before' }] }
        const otherChat = { id: 'chat-b', message: [] }
        const character = { type: 'character', chaId: 'character-a', chatPage: 0, chats: [chat, otherChat] }
        mocks.database = { aiModel: 'test-model', plugins: [{ name: 'end-plugin', script: '' }], characters: [character] }
        mocks.session = new ActiveConversationSession({
            characterId: character.chaId,
            conversationId: chat.id,
            conversation: chat as any,
            storeRevision: 1,
        })
        mocks.selectedTarget = target
        mocks.acquireCompleteConversation.mockReset()
        mocks.acquireCompleteConversation.mockImplementation(async () => ({
            session: mocks.session,
            target,
            release() { releaseCount += 1 },
        }))
        mocks.processSendChat.mockReset()
        await executePluginV3({ name: `end-plugin-${crypto.randomUUID()}`, script: '' } as any)
    })

    afterEach(() => {
        for (const stop of stopListening.splice(0)) stop()
    })

    it('reports a completed generation once after releasing it', async () => {
        const records = listenGenerationEnd()
        mocks.processSendChat.mockImplementation(async () => {
            noteGenerationStarted(target)
            noteGenerationMessage(target, 'reply-1')
            noteGenerationStarted(target)
            noteGenerationMessage(target, 'reply-1')
            return true
        })

        await expect(mocks.api.sendChat('hello')).resolves.toBe(true)

        expect(records).toEqual([{
            ...target,
            status: 'completed',
            reroll: false,
            messageIds: ['reply-1'],
            busy: false,
            released: 1,
        }])
    })

    it('reports failed when generation declines or throws', async () => {
        const records = listenGenerationEnd()
        const failure = new Error('provider failed')
        mocks.processSendChat
            .mockImplementationOnce(async () => {
                noteGenerationStarted(target)
                return false
            })
            .mockImplementationOnce(async () => {
                noteGenerationStarted(target)
                noteGenerationMessage(target, 'error-reply')
                throw failure
            })

        await expect(mocks.api.sendChat('declined')).resolves.toBe(false)
        await expect(mocks.api.sendChat('thrown')).rejects.toBe(failure)

        expect(records.map(({ status, messageIds, busy }) => ({ status, messageIds, busy }))).toEqual([
            { status: 'failed', messageIds: [], busy: false },
            { status: 'failed', messageIds: ['error-reply'], busy: false },
        ])
    })

    it('reports aborted when the selected conversation changes during generation', async () => {
        const records = listenGenerationEnd()
        mocks.processSendChat.mockImplementation(async () => {
            noteGenerationStarted(target)
            mocks.database.characters[0].chatPage = 1
            return false
        })

        await expect(mocks.api.sendChat('hello')).resolves.toBe(false)

        expect(records.map((record) => record.status)).toEqual(['aborted'])
    })

    it('does not report a send that never entered generation', async () => {
        const records = listenGenerationEnd()
        mocks.processSendChat.mockResolvedValue(false)

        await expect(mocks.api.sendChat('hello')).resolves.toBe(false)

        expect(records).toEqual([])
    })
})


it('restarts authority replacements through the full fresh plugin loader once',async()=>{
    const {restartPluginsAfterAuthorityReplacement}=await import('./v3.svelte')
    mocks.authoritativeLoad.mockClear()
    await restartPluginsAfterAuthorityReplacement()
    expect(mocks.authoritativeLoad).toHaveBeenCalledExactlyOnceWith()
})
