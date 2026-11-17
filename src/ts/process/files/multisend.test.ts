import { imageFixture } from '../../../../tests/support/imageFixtures'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'

import { writeFile } from '@tauri-apps/plugin-fs'
import { save } from '@tauri-apps/plugin-dialog'

import { ActiveConversationSession } from '../../storage/activeConversationSession'
import type { PlatformShape } from '../../platform.testSupport'
import type { Chat, Database } from '../../storage/database.svelte'

const mocks = vi.hoisted(() => ({
    dbState: { db: null as Database | null },
    selectedCharacterIndex: 0,
    session: null as ActiveConversationSession | null,
    sendChat: vi.fn(async () => undefined),
    downloadFile: vi.fn(async () => undefined),
    selectedTarget: null as any,
    acquireCompleteConversation: vi.fn(),
    postInlayAsset: vi.fn(),
    InlayInputTooLargeError: class InlayInputTooLargeError extends Error {},
    alertError: vi.fn(),
    platform: { runtime: 'web', os: 'windows' } as PlatformShape,
    pickRewritableIOSFile: vi.fn(async (_name: string) => false),
    rewriteIOSFile: vi.fn(async (_text: string) => {}),
}))

vi.mock('src/ts/stores.svelte', () => ({
    DBState: mocks.dbState,
    selectedCharID: {
        subscribe(run: (value: number) => void) {
            run(mocks.selectedCharacterIndex)
            return () => undefined
        },
    },
}))
vi.mock('../index.svelte', async () => {
    const { doingChat } = await import('../generationState')
    return {
        doingChat,
        sendChat: mocks.sendChat,
    }
})
vi.mock('src/ts/globalApi.svelte', () => ({ downloadFile: mocks.downloadFile }))
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule(() => mocks.platform))
vi.mock('@tauri-apps/plugin-fs', async () => (await import('src/ts/globalApi.testSupport')).tauriFsModule())
vi.mock('@tauri-apps/plugin-dialog', async () => (await import('src/ts/globalApi.testSupport')).tauriDialogModule())
vi.mock('src/ts/storage/iosFiles', () => ({
    pickRewritableIOSFile: mocks.pickRewritableIOSFile,
    rewriteIOSFile: mocks.rewriteIOSFile,
}))
vi.mock('../memory/hypamemory', () => ({
    HypaProcesser: class {
        addText() {}
        async similaritySearch() { return [] }
    },
}))
vi.mock('src/ts/util', () => ({
    BufferToText: (value: Uint8Array) => new TextDecoder().decode(value),
    selectMultipleFile: vi.fn(),
}))
vi.mock('./inlays', () => ({
    postInlayAsset: mocks.postInlayAsset,
    InlayInputTooLargeError: mocks.InlayInputTooLargeError,
}))
vi.mock('src/ts/alert', () => ({ alertError: mocks.alertError }))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({
    captureSelectedConversationTarget: () => mocks.selectedTarget,
    acquireCompleteConversation: mocks.acquireCompleteConversation,
    getActiveConversationSession: () => mocks.session,
    capturePersistentMutationToken: () => undefined,
    acquireDestructiveReplacementFence: () => undefined,
}))

import { postChatFile } from './multisend'
import { InlayInputTooLargeError } from './inlays'
import { doingChat, reserveGeneration } from '../generationState'
import { language } from 'src/lang'
import { selectMultipleFile } from 'src/ts/util'

function createDatabase(): Database {
    const conversation = {
        id: 'conversation-a',
        name: 'Conversation A',
        note: '',
        localLore: [],
        message: [{ role: 'char', data: 'before' }],
    } as Chat
    return {
        characters: [{
            type: 'character',
            chaId: 'character-a',
            chatPage: 0,
            chats: [conversation],
        }],
    } as Database
}

describe('postChatFile PO append', () => {
    beforeEach(() => {
        mocks.dbState.db = createDatabase()
        mocks.selectedCharacterIndex = 0
        mocks.session = null
        mocks.sendChat.mockClear()
        mocks.downloadFile.mockClear()
        mocks.selectedTarget = null
        mocks.acquireCompleteConversation.mockReset()
        mocks.postInlayAsset.mockReset()
        mocks.alertError.mockReset()
        doingChat.set(false)
    })

    it('routes each PO user message through the matching session', async () => {
        const character = mocks.dbState.db!.characters[0]
        const conversation = character.chats[0]
        const onMutation = vi.fn()
        mocks.session = new ActiveConversationSession({
            characterId: character.chaId,
            conversationId: conversation.id,
            conversation,
            storeRevision: 1,
            onMutation,
        })
        const input = new TextEncoder().encode('msgid "hello"\nmsgstr ""\n\n')

        await expect(postChatFile({ name: 'input.po', data: input })).resolves.toEqual([
            { type: 'void' },
        ])

        expect(conversation.message.map((message) => message.data)).toEqual(['before', 'hello'])
        expect(onMutation).toHaveBeenCalledWith(expect.objectContaining({ commands: ['append'] }))
        expect(mocks.sendChat).toHaveBeenCalledTimes(1)
    })

    it('refreshes the target between owned PO session appends', async () => {
        const character = mocks.dbState.db!.characters[0]
        const conversation = character.chats[0]
        mocks.session = new ActiveConversationSession({
            characterId: character.chaId,
            conversationId: conversation.id,
            conversation,
            storeRevision: 1,
        })
        const input = new TextEncoder().encode(
            'msgid "first"\nmsgstr ""\n\nmsgid "second"\nmsgstr ""\n\n',
        )

        await postChatFile({ name: 'input.po', data: input })

        expect(conversation.message.map((message) => message.data)).toEqual([
            'before',
            'first',
            'second',
        ])
        expect(mocks.sendChat).toHaveBeenCalledTimes(2)
    })

    it('promotes before the first PO append and holds the exact lease across generation', async () => {
        const character = mocks.dbState.db!.characters[0]
        const conversation = character.chats[0]
        const session = new ActiveConversationSession({
            characterId: character.chaId,
            conversationId: conversation.id,
            conversation,
            storeRevision: 1,
        })
        mocks.session = session
        const target = { characterId: character.chaId, conversationId: conversation.id }
        mocks.selectedTarget = target
        let resolvePromotion!: (lease: any) => void
        const promotion = new Promise<any>((resolve) => { resolvePromotion = resolve })
        mocks.acquireCompleteConversation.mockReturnValue(promotion)
        let resolveGeneration!: () => void
        mocks.sendChat.mockImplementationOnce(() => new Promise<void>((resolve) => {
            resolveGeneration = resolve
        }))
        let releaseCount = 0
        const input = new TextEncoder().encode('msgid "hello"\nmsgstr ""\n\n')

        const posting = postChatFile({ name: 'input.po', data: input })
        await Promise.resolve()

        expect(conversation.message.map((message) => message.data)).toEqual(['before'])
        expect(mocks.sendChat).not.toHaveBeenCalled()

        const pin = session.acquirePin('compatibility')
        resolvePromotion({
            session,
            target,
            release() {
                releaseCount += 1
                pin.release()
            },
        })
        while (mocks.sendChat.mock.calls.length === 0) await Promise.resolve()

        expect(conversation.message.map((message) => message.data)).toEqual(['before', 'hello'])
        expect(session.pinCount('compatibility')).toBe(1)

        resolveGeneration()
        await expect(posting).resolves.toEqual([{ type: 'void' }])
        expect(releaseCount).toBe(1)
        expect(session.pinCount('compatibility')).toBe(0)
    })

    it('aborts a PO append when the promoted lease does not own the selected conversation', async () => {
        const character = mocks.dbState.db!.characters[0]
        const conversation = character.chats[0]
        const otherConversation = { ...conversation, id: 'chat-b', message: [] } as Chat
        const otherSession = new ActiveConversationSession({
            characterId: character.chaId,
            conversationId: otherConversation.id,
            conversation: otherConversation,
            storeRevision: 1,
        })
        const target = { characterId: character.chaId, conversationId: conversation.id }
        mocks.selectedTarget = target
        let releaseCount = 0
        mocks.acquireCompleteConversation.mockResolvedValue({
            session: otherSession,
            target,
            release() { releaseCount += 1 },
        })
        const input = new TextEncoder().encode('msgid "hello"\nmsgstr ""\n\n')

        await expect(postChatFile({ name: 'input.po', data: input })).resolves.toEqual([
            { type: 'void' },
        ])

        expect(conversation.message.map((message) => message.data)).toEqual(['before'])
        expect(mocks.sendChat).not.toHaveBeenCalled()
        expect(releaseCount).toBe(1)
    })

    it('does not cancel a provider-waiting generation or append when PO overlaps it', async () => {
        const conversation = mocks.dbState.db!.characters[0].chats[0]
        const providerGeneration = reserveGeneration()
        expect(providerGeneration).not.toBeNull()
        doingChat.set(false)
        const input = new TextEncoder().encode('msgid "overlap"\nmsgstr ""\n\n')

        try {
            await expect(postChatFile({ name: 'input.po', data: input })).resolves.toEqual([
                { type: 'void' },
            ])

            expect(providerGeneration!.isCurrent()).toBe(true)
            expect(get(doingChat)).toBe(true)
            expect(conversation.message.map((message) => message.data)).toEqual(['before'])
            expect(mocks.acquireCompleteConversation).not.toHaveBeenCalled()
            expect(mocks.sendChat).not.toHaveBeenCalled()
        } finally {
            providerGeneration?.release()
        }
    })
})

describe('postChatFile PO save destination', () => {
    const input = new TextEncoder().encode('msgid "first"\nmsgstr ""\n\nmsgid "second"\nmsgstr ""\n\n')
    const afterFirst = 'msgid "first"\nmsgstr ""\n"first"\n\n'
    const afterSecond = `${afterFirst}msgid "second"\nmsgstr ""\n"second"\n\n`
    const translated = `${afterSecond}\n`
    const decode = (data: unknown) => new TextDecoder().decode(data as Uint8Array)
    let conversation: Chat

    beforeEach(() => {
        mocks.dbState.db = createDatabase()
        mocks.selectedCharacterIndex = 0
        mocks.selectedTarget = null
        const character = mocks.dbState.db.characters[0]
        conversation = character.chats[0]
        mocks.session = new ActiveConversationSession({
            characterId: character.chaId,
            conversationId: conversation.id,
            conversation,
            storeRevision: 1,
        })
        mocks.sendChat.mockReset().mockImplementation(async () => undefined)
        mocks.downloadFile.mockClear()
        mocks.pickRewritableIOSFile.mockReset()
        mocks.rewriteIOSFile.mockReset()
        vi.mocked(save).mockReset()
        vi.mocked(writeFile).mockReset()
        doingChat.set(false)
    })

    afterEach(() => {
        mocks.platform = { runtime: 'web', os: 'windows' }
    })

    it.each<[string, PlatformShape, string]>([
        ['desktop', { runtime: 'tauri', os: 'windows' }, '/synthetic/translated.po'],
        ['Android', { runtime: 'tauri', os: 'android' }, 'content://synthetic/translated.po'],
    ])('asks for the %s file once and rewrites it after every message', async (_name, platform, path) => {
        mocks.platform = platform
        vi.mocked(save).mockResolvedValue(path)

        await expect(postChatFile({ name: 'input.po', data: input })).resolves.toEqual([{ type: 'void' }])

        expect(vi.mocked(save).mock.calls).toEqual([[{ defaultPath: 'translated.po' }]])
        expect(vi.mocked(writeFile).mock.calls.map(([target, data]) => [target, decode(data)])).toEqual([
            [path, afterFirst],
            [path, afterSecond],
            [path, translated],
        ])
        expect(mocks.downloadFile).not.toHaveBeenCalled()
    })

    it('asks for the iOS file once and rewrites it after every message', async () => {
        mocks.platform = { runtime: 'tauri', os: 'ios' }
        mocks.pickRewritableIOSFile.mockResolvedValue(true)

        await postChatFile({ name: 'input.po', data: input })

        expect(mocks.pickRewritableIOSFile.mock.calls).toEqual([['translated.po']])
        expect(mocks.rewriteIOSFile.mock.calls).toEqual([[afterFirst], [afterSecond], [translated]])
        expect(save).not.toHaveBeenCalled()
        expect(mocks.downloadFile).not.toHaveBeenCalled()
    })

    it('keeps the finished messages in the Android file when the run stops partway', async () => {
        mocks.platform = { runtime: 'tauri', os: 'android' }
        vi.mocked(save).mockResolvedValue('content://synthetic/translated.po')
        const failure = new Error('provider failed')
        mocks.sendChat.mockResolvedValueOnce(undefined).mockRejectedValueOnce(failure)

        await expect(postChatFile({ name: 'input.po', data: input })).rejects.toBe(failure)

        expect(vi.mocked(writeFile).mock.calls.map(([, data]) => decode(data))).toEqual([afterFirst])
    })

    it.each<[string, PlatformShape, () => void]>([
        ['desktop', { runtime: 'tauri', os: 'windows' }, () => vi.mocked(save).mockResolvedValue(null)],
        ['Android', { runtime: 'tauri', os: 'android' }, () => vi.mocked(save).mockResolvedValue(null)],
        ['iOS', { runtime: 'tauri', os: 'ios' }, () => mocks.pickRewritableIOSFile.mockResolvedValue(false)],
    ])('sends nothing when the %s destination is cancelled', async (_name, platform, cancel) => {
        mocks.platform = platform
        cancel()

        await expect(postChatFile({ name: 'input.po', data: input })).resolves.toEqual([{ type: 'void' }])

        expect(conversation.message.map((message) => message.data)).toEqual(['before'])
        expect(mocks.sendChat).not.toHaveBeenCalled()
        expect(writeFile).not.toHaveBeenCalled()
        expect(mocks.rewriteIOSFile).not.toHaveBeenCalled()
        const next = reserveGeneration()
        expect(next).not.toBeNull()
        next?.release()
    })

    it('saves the finished file once on the web', async () => {
        await postChatFile({ name: 'input.po', data: input })

        expect(mocks.downloadFile.mock.calls).toEqual([['translated.po', translated]])
        expect(save).not.toHaveBeenCalled()
    })
})

describe('postChatFile attachment errors', () => {
    beforeEach(() => {
        mocks.dbState.db = createDatabase()
        mocks.sendChat.mockClear()
        mocks.postInlayAsset.mockReset()
        mocks.alertError.mockReset()
    })

    it.each([false, true])('does not repeat selection failures when attachment storage fails: %s', async storageFails => {
        vi.mocked(selectMultipleFile).mockImplementationOnce(async (_extensions, options) => {
            options!.failures!.push('rejected.png: damaged')
            mocks.alertError('rejected.png: damaged')
            return [{ name: 'valid.png', data: imageFixture('png') }]
        })
        if (storageFails) mocks.postInlayAsset.mockRejectedValueOnce(new Error('cannot store'))
        else mocks.postInlayAsset.mockResolvedValueOnce('inlay-id')
        expect(await postChatFile('')).toEqual(storageFails ? [] : [{ type: 'asset', data: 'inlay-id' }])
        expect(mocks.alertError.mock.calls.map(([message]) => message)).toEqual(storageFails
            ? ['rejected.png: damaged', 'valid.png: cannot store'] : ['rejected.png: damaged'])
    })

    it.each(['gif', 'png'])('skips an oversized %s attachment without appending an asset or message', async (extension) => {
        mocks.postInlayAsset.mockRejectedValueOnce(new InlayInputTooLargeError('over the input limit'))
        const conversation = mocks.dbState.db!.characters[0].chats[0]

        await expect(postChatFile({ name: `huge.${extension}`, data: imageFixture(extension as 'png' | 'gif') })).resolves.toEqual([])

        expect(mocks.alertError).toHaveBeenCalledWith(`huge.${extension}: ${language.risuNest.inlay.tooLarge}`)
        expect(conversation.message).toEqual([{ role: 'char', data: 'before' }])
        expect(mocks.sendChat).not.toHaveBeenCalled()
    })

    it.each(['png', 'mp3', 'txt'])('reports ordinary %s attachment failures by name', async (extension) => {
        const failure = new Error('storage failed')
        mocks.postInlayAsset.mockRejectedValueOnce(failure)

        await expect(postChatFile({ name: `attachment.${extension}`, data: extension === 'mp3' ? new Uint8Array([1]) : imageFixture('png') }))
            .resolves.toEqual([])

        expect(mocks.alertError).toHaveBeenCalledWith(`attachment.${extension}: storage failed`)
    })

    it('reports an unrecognized attachment instead of silently skipping it', async () => {
        mocks.postInlayAsset.mockResolvedValueOnce(null)

        await expect(postChatFile({ name: 'attachment.avi', data: new Uint8Array([1]) }))
            .resolves.toEqual([])

        expect(mocks.alertError).toHaveBeenCalledWith(`attachment.avi: ${language.risuNest.importDialog.reasonUnsupportedFormat}`)
    })
})
