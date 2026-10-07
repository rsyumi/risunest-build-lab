import { beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'

const mocks = vi.hoisted(() => ({
    unexpectedNativeRuntimeAccess: () => {
        throw new Error('Unexpected native runtime access in this test')
    },
    database: { characters: [] as any[] },
    nextId: 0,
    navigationGeneration: 0,
    activateCharacter: vi.fn(async (_id?: string, _options?: any) => true),
    deactivateActiveWorkingSet: vi.fn(async () => true),
    getPersistentNavigationGeneration: vi.fn(() => mocks.navigationGeneration),
    fencePersistentNavigation: vi.fn(() => ++mocks.navigationGeneration),
    invalidatePersistentNavigation: vi.fn(() => {
        mocks.navigationGeneration++
    }),
    commitCharacterAddition: vi.fn(async (request: any, _reason: string) => request.install()),
    markPersistentDataDirty: vi.fn(),
    mutatePersistentCharacterDetail: vi.fn(),
    deletePersistentCharacter: vi.fn(),
    materializePersistentDatabaseSnapshotWithRevision: vi.fn(),
    replacePersistentDatabase: vi.fn(),
    readPersistentCharacterDetail: vi.fn(),
    readPersistentCompleteCharacter: vi.fn(),
    readPersistentConversation: vi.fn(),
    replacePersistentCompleteCharacter: vi.fn(),
    reconcilePersistentActiveCharacterIds: vi.fn(),
    captureSelectedConversationTarget: vi.fn((): any => null),
    acquireCompleteConversation: vi.fn(),
    flushPendingData: vi.fn(async (_reason: string) => {}),
    getSelectedConversationMode: vi.fn((): 'complete' | 'windowed' | null => null),
    editWindowedChatList: vi.fn(),
    alertConfirm: vi.fn(async (_options?: unknown) => true),
    alertSelect: vi.fn(async () => '0'),
    alertAddCharacter: vi.fn(async () => 'createfromScratch'),
    alertError: vi.fn(),
    alertToast: vi.fn(),
    changeChatTo: vi.fn(async (_idOrIndex?: string | number) => true),
    downloadFile: vi.fn(),
    findCharacterbyId: vi.fn(),
    yieldToUi: vi.fn(async () => {}),
    saveImage: vi.fn(async () => 'assets/synthetic.png'),
    selectSingleFile: vi.fn(),
    selectMultipleFile: vi.fn(),
}))

vi.mock('uuid', () => ({
    v4: () => `generated-${++mocks.nextId}`,
}))
vi.mock('./storage/database.svelte', () => ({
    saveImage: mocks.saveImage,
    defaultSdDataFunc: () => ({}),
    getDatabase: (options?: { snapshot?: boolean }) => options?.snapshot
        ? structuredClone(mocks.database)
        : mocks.database,
    getCharacterByIndex: (index: number) => mocks.database.characters[index],
    setCharacterByIndex: (index: number, character: unknown) => {
        mocks.database.characters[index] = character
    },
}))
vi.mock('./alert', async () => {
    const { writable } = await import('svelte/store')
    return {
        alertAddCharacter: mocks.alertAddCharacter,
        alertConfirm: mocks.alertConfirm,
        alertCheckboxConfirm: async (options: unknown) => ({ confirmed: await mocks.alertConfirm(options), checked: true }),
        alertError: mocks.alertError,
        alertToast: mocks.alertToast,
        alertNormal: vi.fn(),
        alertSelect: mocks.alertSelect,
        alertStore: writable({ type: 'none', msg: '' }),
        alertWait: vi.fn(),
    }
})
vi.mock('../lang', () => ({ language: { errors: {}, checkboxConfirmation: {
    characterDeletion: "Delete character",
    characterTrashDescription: "Moves to the trash.",
    characterDeletionDescription: "Deleted permanently.",
} } }))
vi.mock('./util', () => ({
    checkNullish: (value: unknown) => value === null || value === undefined,
    findCharacterbyId: mocks.findCharacterbyId,
    findCharacterIndexbyId: vi.fn(),
    getUserName: vi.fn(),
    selectMultipleFile: mocks.selectMultipleFile,
    selectSingleFile: mocks.selectSingleFile,
}))
vi.mock('./media', () => ({ getImageType: vi.fn() }))
vi.mock('./stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return {
        DBState: { db: mocks.database },
        MobileGUIStack: writable(0),
        OpenRealmStore: writable(false),
        selectedCharID: writable(-1),
    }
})
vi.mock('./globalApi.svelte', () => ({
    AppendableBuffer: class {},
    changeChatTo: mocks.changeChatTo,
    checkCharOrder: vi.fn(),
    createChatCopyName: (name: string, type: string) => `${name} (${type})`,
    downloadFile: mocks.downloadFile,
    getFileSrc: vi.fn(),
}))
vi.mock('./process/inlayScreen', () => ({ updateInlayScreen: (value: unknown) => value }))
vi.mock('./parser/parser.svelte', () => ({ parseMarkdownSafe: (value: string) => value }))
vi.mock('./translator/translator', () => ({ translateHTML: vi.fn() }))
vi.mock('./process/index.svelte', async () => {
    const { writable } = await import('svelte/store')
    return { doingChat: writable(false) }
})
vi.mock('./characterCards', () => ({ importCharacter: vi.fn() }))
vi.mock('./pngChunk', () => ({ PngChunk: {} }))
vi.mock('./ui/yieldToUi', () => ({ yieldToUi: mocks.yieldToUi }))
vi.mock('./storage/persistentDataRuntime.svelte', () => ({
    activateCharacter: mocks.activateCharacter,
    acquireDestructiveReplacementFence: mocks.unexpectedNativeRuntimeAccess,
    capturePersistentMutationToken: mocks.unexpectedNativeRuntimeAccess,
    commitCharacterAddition: mocks.commitCharacterAddition,
    deactivateActiveWorkingSet: mocks.deactivateActiveWorkingSet,
    fencePersistentNavigation: mocks.fencePersistentNavigation,
    getPersistentNavigationGeneration: mocks.getPersistentNavigationGeneration,
    invalidatePersistentNavigation: mocks.invalidatePersistentNavigation,
    markPersistentDataDirty: mocks.markPersistentDataDirty,
    mutatePersistentCharacterDetail: mocks.mutatePersistentCharacterDetail,
    deletePersistentCharacter:
        mocks.deletePersistentCharacter,
    materializePersistentDatabaseSnapshotWithRevision:
        mocks.materializePersistentDatabaseSnapshotWithRevision,
    replacePersistentDatabase: mocks.replacePersistentDatabase,
    readPersistentCharacterDetail: mocks.readPersistentCharacterDetail,
    readPersistentCompleteCharacter: mocks.readPersistentCompleteCharacter,
    readPersistentConversation: mocks.readPersistentConversation,
    replacePersistentCompleteCharacter: mocks.replacePersistentCompleteCharacter,
    reconcilePersistentActiveCharacterIds: mocks.reconcilePersistentActiveCharacterIds,
    captureSelectedConversationTarget: mocks.captureSelectedConversationTarget,
    acquireCompleteConversation: mocks.acquireCompleteConversation,
    flushPendingData: mocks.flushPendingData,
    getSelectedConversationMode: mocks.getSelectedConversationMode,
    editWindowedChatList: mocks.editWindowedChatList,
}))

import {
    addCharacter,
    addNewChat,
    changeChar,
    characterFormatUpdate,
    createBlankChar,
    createNewCharacter,
    commitDetachedCharacter,
    duplicateChat,
    editSelectedChatList,
    exportAllChats,
    exportChat,
    importChat,
    removeChar,
    removeChat,
    selectCharImg,
    addCharEmotion,
    addingEmotion,
} from './characters'
import { createMetadataOnlySelectedConversation } from './storage/selectedConversationLifecycle'
import { SelectedConversationPromotionStaleError } from './storage/activeWorkingSet.svelte'
import { MobileGUIStack, OpenRealmStore, selectedCharID } from './stores.svelte'
import { doingChat } from './process/index.svelte'
import { createConversationSummaryStub } from './storage/conversationResidency'
import {
    beginNavigationActivity,
    navigationActivity,
} from './ui/navigationActivity'

function deferred<T>() {
    let resolve!: (value: T) => void
    const promise = new Promise<T>((resolvePromise) => {
        resolve = resolvePromise
    })
    return { promise, resolve }
}

describe('runtime chat identity', () => {
    it.each(['portrait', 'emotion'] as const)('applies a pending %s import to its original character after reordering', async (kind) => {
        const first = { type: 'character', chaId: 'first', image: 'assets/old.png', emotionImages: [], chats: [] }
        const second = { type: 'character', chaId: 'second', image: '', emotionImages: [], chats: [] }
        mocks.database.characters = [first, second]
        const chosen = deferred<any>()
        if (kind === 'portrait') mocks.selectSingleFile.mockReturnValueOnce(chosen.promise)
        else mocks.selectMultipleFile.mockReturnValueOnce(chosen.promise)
        const pending = kind === 'portrait' ? selectCharImg(0) : addCharEmotion(0)
        mocks.database.characters.reverse()
        const file = { name: 'synthetic.png', data: new Uint8Array([1]) }
        chosen.resolve(kind === 'portrait' ? file : [file])
        await pending
        expect(second).toMatchObject({ image: '', emotionImages: [] })
        if (kind === 'portrait') {
            expect(first.image).toBe('assets/synthetic.png')
            expect((first as any).ccAssets[0].uri).toBe('assets/old.png')
        } else expect(first.emotionImages).toEqual([['synthetic', 'assets/synthetic.png']])
    })

    it('releases emotion import state after a failed asset save', async () => {
        mocks.database.characters = [{ type: 'character', chaId: 'first', emotionImages: [], chats: [] }]
        mocks.selectMultipleFile.mockResolvedValueOnce([{ name: 'synthetic.png', data: new Uint8Array([1]) }])
        mocks.saveImage.mockRejectedValueOnce(new Error('Synthetic save failure'))
        await expect(addCharEmotion(0)).rejects.toThrow('Synthetic save failure')
        expect(get(addingEmotion)).toBe(false)
    })

    beforeEach(() => {
        mocks.database.characters = []
        mocks.nextId = 0
        mocks.navigationGeneration = 0
        OpenRealmStore.set(false)
        doingChat.set(false)
        vi.clearAllMocks()
        mocks.activateCharacter.mockResolvedValue(true)
        mocks.deactivateActiveWorkingSet.mockResolvedValue(true)
        mocks.alertConfirm.mockResolvedValue(true)
        mocks.alertSelect.mockResolvedValue('0')
        mocks.alertAddCharacter.mockResolvedValue('createfromScratch')
        mocks.commitCharacterAddition.mockImplementation(async (request) => request.install())
        mocks.yieldToUi.mockResolvedValue(undefined)
        mocks.mutatePersistentCharacterDetail.mockImplementation(async (id, _reason, mutate) => {
            const index = mocks.database.characters.findIndex((character) => character.chaId === id)
            if (index < 0) return false
            const { chats: _chats, ...detail } = structuredClone(mocks.database.characters[index])
            const state = { root: {}, character: detail }
            const result = await mutate(state)
            Object.assign(mocks.database, state.root)
            if (result?.delete) mocks.database.characters.splice(index, 1)
            else Object.assign(mocks.database.characters[index], state.character)
            return true
        })
        mocks.deletePersistentCharacter.mockImplementation(async (id) => {
            const index = mocks.database.characters.findIndex((character) => character.chaId === id)
            if (index < 0) return false
            mocks.database.characters.splice(index, 1)
            return true
        })
        mocks.materializePersistentDatabaseSnapshotWithRevision.mockImplementation(async () => ({
            database: structuredClone(mocks.database),
            revision: 1,
            mutationGeneration: 0,
        }))
        mocks.replacePersistentDatabase.mockImplementation(async (database) => {
            Object.assign(mocks.database, structuredClone(database))
        })
        mocks.readPersistentCharacterDetail.mockImplementation(async (id) => {
            const character = mocks.database.characters.find((candidate) => candidate.chaId === id)
            if (!character) return null
            const { chats: _chats, ...detail } = structuredClone(character)
            return detail
        })
        mocks.replacePersistentCompleteCharacter.mockImplementation(async (id, _reason, mutate) => {
            const index = mocks.database.characters.findIndex((character) => character.chaId === id)
            if (index < 0) return false
            mocks.database.characters[index] = await mutate(
                structuredClone(mocks.database.characters[index]),
            )
            return true
        })
    })

    it('navigates by the character ID captured before hydration', async () => {
        const first = createBlankChar()
        const second = createBlankChar()
        mocks.database.characters.push(first, second)
        mocks.activateCharacter.mockImplementation(async () => {
            mocks.database.characters.reverse()
            return true
        })

        const changed = await changeChar(0)

        expect(changed).toBe(true)
        expect(mocks.activateCharacter).toHaveBeenCalledWith(first.chaId, {
            normalize: expect.any(Function),
        })
    })

    it('publishes character navigation before yielding to the UI', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        const paint = deferred<void>()
        mocks.yieldToUi.mockReturnValueOnce(paint.promise)

        const pending = changeChar(0)

        expect(get(navigationActivity)?.kind).toBe('character')
        expect(mocks.fencePersistentNavigation).toHaveBeenCalledOnce()
        expect(mocks.activateCharacter).not.toHaveBeenCalled()

        paint.resolve()
        await expect(pending).resolves.toBe(true)
        expect(get(navigationActivity)).toBeNull()
    })

    it('keeps the open character without reloading it', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.captureSelectedConversationTarget.mockReturnValueOnce({
            characterId: character.chaId,
        })
        const reseter = vi.fn()
        doingChat.set(true)
        try {
            await expect(changeChar(0, { reseter })).resolves.toBe(true)
        } finally {
            doingChat.set(false)
            selectedCharID.set(-1)
        }

        expect(reseter).toHaveBeenCalledOnce()
        expect(mocks.fencePersistentNavigation).not.toHaveBeenCalled()
        expect(mocks.activateCharacter).not.toHaveBeenCalled()
        expect(get(navigationActivity)).toBeNull()
    })

    it('activates the selected index when another character is open', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.captureSelectedConversationTarget.mockReturnValueOnce({
            characterId: 'another-character',
        })
        try {
            await expect(changeChar(0)).resolves.toBe(true)
        } finally {
            selectedCharID.set(-1)
        }

        expect(mocks.fencePersistentNavigation).toHaveBeenCalledOnce()
        expect(mocks.activateCharacter).toHaveBeenCalledOnce()
    })

    it('clears character navigation when the screen reset fails', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)

        await expect(
            changeChar(0, {
                reseter: () => {
                    throw new Error('reset failed')
                },
            }),
        ).resolves.toBe(false)
        expect(get(navigationActivity)).toBeNull()
        expect(mocks.activateCharacter).not.toHaveBeenCalled()
    })

    it('normalizes navigation metadata without enumerating the selected chat body', async () => {
        const character = createBlankChar()
        const messages: any[] = []
        Object.defineProperty(messages, '0', {
            configurable: true,
            enumerable: true,
            get: () => {
                throw new Error('navigation enumerated the message body')
            },
        })
        messages.length = 1
        character.chats[0].message = messages
        mocks.database.characters.push(character)
        const { chats: _chats, ...detail } = character
        mocks.readPersistentCharacterDetail.mockResolvedValue(detail)
        mocks.activateCharacter.mockImplementationOnce(async (_id, options) => {
            options.normalize(character)
            return true
        })

        await expect(changeChar(0)).resolves.toBe(true)
        expect(mocks.markPersistentDataDirty).not.toHaveBeenCalled()
    })

    it('keeps a newer conversation visible while its UI yield outlives an older character activation', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        const activation = deferred<boolean>()
        const conversationPaint = deferred<void>()
        mocks.activateCharacter.mockReturnValueOnce(activation.promise)

        const older = changeChar(0)
        await vi.waitFor(() =>
            expect(mocks.activateCharacter).toHaveBeenCalledOnce(),
        )
        const newer = beginNavigationActivity('conversation')
        const newerPending = conversationPaint.promise.finally(() =>
            newer.finish(),
        )
        activation.resolve(true)

        await expect(older).resolves.toBe(false)
        expect(mocks.markPersistentDataDirty).not.toHaveBeenCalled()
        expect(get(navigationActivity)?.kind).toBe('conversation')
        conversationPaint.resolve()
        await newerPending
        expect(get(navigationActivity)).toBeNull()
    })

    it('commits a stable-ID character deletion through one authoritative replacement', async () => {
        const first = createBlankChar()
        const second = createBlankChar()
        mocks.database.characters.push(first, second)

        await removeChar(first.chaId, first.name, 'permanentForce')

        expect(mocks.deletePersistentCharacter).toHaveBeenCalledWith(
            first.chaId,
            'character-removal',
        )
        expect(mocks.replacePersistentDatabase).not.toHaveBeenCalled()
        expect(mocks.database.characters.map((character: any) => character.chaId)).toEqual([second.chaId])
        expect(mocks.deactivateActiveWorkingSet).toHaveBeenCalledTimes(2)
    })

    it.each([
        ['normal', 'Moves to the trash.'],
        ['permanent', 'Deleted permanently.'],
    ] as const)('describes a %s removal by what it does', async (type, description) => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        mocks.alertConfirm.mockResolvedValueOnce(false)

        await removeChar(character.chaId, character.name, type)

        expect(mocks.alertConfirm).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ description }))
    })

    it('removes the character ID captured before confirmation', async () => {
        const first = createBlankChar()
        const second = createBlankChar()
        mocks.database.characters.push(first, second)
        mocks.alertConfirm.mockImplementationOnce(async () => {
            mocks.database.characters.reverse()
            return true
        }).mockResolvedValueOnce(true)

        await removeChar(0, first.name, 'permanent')

        expect(mocks.deletePersistentCharacter).toHaveBeenCalledWith(
            first.chaId,
            'character-removal',
        )
        expect(mocks.database.characters.map((character: any) => character.chaId)).toEqual([second.chaId])
    })

    it('does not trash a selected character when safe deactivation fails', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.deactivateActiveWorkingSet.mockResolvedValueOnce(false)

        await removeChar(character.chaId, character.name, 'normal')

        expect(mocks.database.characters[0].trashTime).toBeUndefined()
        expect(get(selectedCharID)).toBe(0)
        expect(mocks.deactivateActiveWorkingSet).toHaveBeenCalledOnce()
        expect(mocks.mutatePersistentCharacterDetail).not.toHaveBeenCalled()
    })

    it('keeps selection when the selected-character trash mutation does not commit', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.mutatePersistentCharacterDetail.mockResolvedValueOnce(false)

        await removeChar(character.chaId, character.name, 'normal')

        expect(mocks.deactivateActiveWorkingSet).toHaveBeenCalledOnce()
        expect(mocks.mutatePersistentCharacterDetail).toHaveBeenCalledOnce()
        expect(get(selectedCharID)).toBe(0)
        expect(mocks.activateCharacter).toHaveBeenCalledWith(character.chaId)
    })

    describe('when releasing the selected character clears the selection', () => {
        beforeEach(() => {
            mocks.deactivateActiveWorkingSet.mockImplementation(async () => {
                selectedCharID.set(-1)
                return true
            })
            mocks.activateCharacter.mockImplementation(async (id) => {
                selectedCharID.set(mocks.database.characters.findIndex((candidate) => candidate.chaId === id))
                return true
            })
        })

        it('reopens the character when the trash mutation does not commit', async () => {
            const character = createBlankChar()
            mocks.database.characters.push(createBlankChar(), character)
            selectedCharID.set(1)
            mocks.mutatePersistentCharacterDetail.mockResolvedValueOnce(false)

            await removeChar(character.chaId, character.name, 'normal')

            expect(mocks.activateCharacter).toHaveBeenCalledExactlyOnceWith(character.chaId)
            expect(get(selectedCharID)).toBe(1)
            expect(mocks.database.characters[1].trashTime).toBeUndefined()
        })

        it('reconciles the empty selection when reopening fails', async () => {
            const character = createBlankChar()
            mocks.database.characters.push(character)
            selectedCharID.set(0)
            mocks.mutatePersistentCharacterDetail.mockResolvedValueOnce(false)
            mocks.activateCharacter.mockResolvedValue(false)

            await removeChar(character.chaId, character.name, 'normal')

            expect(mocks.activateCharacter).toHaveBeenCalledWith(character.chaId)
            expect(get(selectedCharID)).toBe(-1)
            expect(mocks.reconcilePersistentActiveCharacterIds).toHaveBeenCalledWith(
                mocks.database,
                null,
            )
        })

        it('does not reopen the character after navigation moved on', async () => {
            const character = createBlankChar()
            mocks.database.characters.push(character)
            selectedCharID.set(0)
            const mutation = deferred<boolean>()
            mocks.mutatePersistentCharacterDetail.mockReturnValueOnce(mutation.promise)

            const removal = removeChar(character.chaId, character.name, 'normal')
            await vi.waitFor(() => expect(mocks.mutatePersistentCharacterDetail).toHaveBeenCalledOnce())
            mocks.navigationGeneration++
            mutation.resolve(false)
            await removal

            expect(mocks.activateCharacter).not.toHaveBeenCalled()
            expect(get(selectedCharID)).toBe(-1)
        })

        it('keeps the selection empty after the trash mutation commits', async () => {
            const character = createBlankChar()
            mocks.database.characters.push(character)
            selectedCharID.set(0)

            await removeChar(character.chaId, character.name, 'normal')

            expect(mocks.database.characters[0].trashTime).toEqual(expect.any(Number))
            expect(mocks.activateCharacter).not.toHaveBeenCalled()
            expect(get(selectedCharID)).toBe(-1)
            expect(mocks.reconcilePersistentActiveCharacterIds).toHaveBeenCalledWith(
                mocks.database,
                null,
            )
        })
    })

    it('deselects a released stub when busy generation blocks failure restoration', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        const mutation = deferred<boolean>()
        mocks.mutatePersistentCharacterDetail.mockReturnValueOnce(mutation.promise)
        mocks.activateCharacter.mockResolvedValueOnce(false)

        const removal = removeChar(character.chaId, character.name, 'normal')
        await vi.waitFor(() => expect(mocks.mutatePersistentCharacterDetail).toHaveBeenCalledOnce())
        doingChat.set(true)
        mutation.resolve(false)
        await removal

        expect(mocks.activateCharacter).toHaveBeenCalledWith(character.chaId)
        expect(get(selectedCharID)).toBe(-1)
        expect(mocks.reconcilePersistentActiveCharacterIds).toHaveBeenCalledWith(
            mocks.database,
            null,
        )
    })

    it('deselects a released stub when failure restoration rejects', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.mutatePersistentCharacterDetail.mockResolvedValueOnce(false)
        mocks.activateCharacter.mockRejectedValueOnce(new Error('restore read failed'))

        await expect(
            removeChar(character.chaId, character.name, 'normal'),
        ).resolves.toBeUndefined()

        expect(get(selectedCharID)).toBe(-1)
        expect(mocks.reconcilePersistentActiveCharacterIds).toHaveBeenCalledWith(
            mocks.database,
            null,
        )
    })

    it('preserves the mutation error when failure restoration also rejects', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.mutatePersistentCharacterDetail.mockRejectedValueOnce(
            new Error('mutation failed'),
        )
        mocks.activateCharacter.mockRejectedValueOnce(new Error('restore read failed'))

        await expect(
            removeChar(character.chaId, character.name, 'normal'),
        ).rejects.toThrow('mutation failed')

        expect(get(selectedCharID)).toBe(-1)
        expect(mocks.reconcilePersistentActiveCharacterIds).toHaveBeenCalledWith(
            mocks.database,
            null,
        )
    })

    it('does not restore an old selection after a newer navigation wins', async () => {
        const first = createBlankChar()
        const second = createBlankChar()
        mocks.database.characters.push(first, second)
        selectedCharID.set(0)
        const mutation = deferred<boolean>()
        mocks.mutatePersistentCharacterDetail.mockReturnValueOnce(mutation.promise)

        const removal = removeChar(first.chaId, first.name, 'normal')
        await vi.waitFor(() => expect(mocks.mutatePersistentCharacterDetail).toHaveBeenCalledOnce())
        mocks.navigationGeneration++
        selectedCharID.set(1)
        mutation.resolve(false)
        await removal

        expect(get(selectedCharID)).toBe(1)
        expect(mocks.activateCharacter).not.toHaveBeenCalled()
    })

    it('does not clear a newer selection when delayed removal succeeds', async () => {
        const first = createBlankChar()
        const second = createBlankChar()
        mocks.database.characters.push(first, second)
        selectedCharID.set(0)
        const mutation = deferred<boolean>()
        mocks.mutatePersistentCharacterDetail.mockReturnValueOnce(mutation.promise)

        const removal = removeChar(first.chaId, first.name, 'normal')
        await vi.waitFor(() => expect(mocks.mutatePersistentCharacterDetail).toHaveBeenCalledOnce())
        mocks.navigationGeneration++
        selectedCharID.set(1)
        mutation.resolve(true)
        await removal

        expect(get(selectedCharID)).toBe(1)
        expect(mocks.reconcilePersistentActiveCharacterIds).not.toHaveBeenCalled()
    })

    it('clears selection only after the selected-character trash mutation commits', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)

        await removeChar(character.chaId, character.name, 'normal')

        expect(mocks.deactivateActiveWorkingSet.mock.invocationCallOrder[0]).toBeLessThan(
            mocks.mutatePersistentCharacterDetail.mock.invocationCallOrder[0],
        )
        expect(mocks.database.characters[0].trashTime).toEqual(expect.any(Number))
        expect(get(selectedCharID)).toBe(-1)
        expect(mocks.reconcilePersistentActiveCharacterIds).toHaveBeenCalledWith(
            mocks.database,
            null,
        )
    })

    it('keeps a locally committed trash when official publication fails', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.mutatePersistentCharacterDetail.mockImplementationOnce(
            async (id, _reason, mutate) => {
                const target = mocks.database.characters.find((candidate) => candidate.chaId === id)
                const { chats: _chats, ...detail } = structuredClone(target)
                await mutate({ root: {}, character: detail })
                Object.assign(target, detail)
                throw new Error('official publish failed')
            },
        )

        await expect(
            removeChar(character.chaId, character.name, 'normal'),
        ).rejects.toThrow('official publish failed')

        expect(mocks.database.characters[0].trashTime).toEqual(expect.any(Number))
        expect(get(selectedCharID)).toBe(-1)
        expect(mocks.activateCharacter).not.toHaveBeenCalled()
    })

    it('assigns an ID before a new character first chat is inserted', () => {
        const character = createBlankChar()

        expect(character.chats[0].id).toBeTruthy()
    })

    it('commits a detached scratch character before installing it', async () => {
        let installedDuringCommit = false
        mocks.commitCharacterAddition.mockImplementation(async (request) => {
            expect(mocks.database.characters).toEqual([])
            expect(request.characterId).toBeTruthy()
            request.install()
            installedDuringCommit = mocks.database.characters.length === 1
        })

        const characterId = await createNewCharacter()

        expect(installedDuringCommit).toBe(true)
        expect(characterId).toBe(mocks.database.characters[0].chaId)
        expect(mocks.database.characters[0].chats.every((chat) => chat.id)).toBe(true)
    })

    it('preserves an installed dirty character when addition publication fails', async () => {
        mocks.commitCharacterAddition.mockImplementation(async (request) => {
            request.install()
            throw new Error('publication failed')
        })

        await expect(createNewCharacter()).rejects.toThrow('publication failed')

        expect(mocks.database.characters).toHaveLength(1)
        expect(mocks.commitCharacterAddition).toHaveBeenCalledOnce()
    })

    it('activates the captured scratch character only after its commit succeeds', async () => {
        const events: string[] = []
        mocks.commitCharacterAddition.mockImplementation(async (request) => {
            events.push('commit')
            request.install()
        })
        mocks.activateCharacter.mockImplementation(async (id) => {
            events.push(`activate:${id}`)
            return true
        })

        await addCharacter()

        const characterId = mocks.database.characters[0].chaId
        expect(events).toEqual(['commit', `activate:${characterId}`])
    })

    it.each([
        ['local', 'createfromScratch'],
    ])('settles a %s addition rejection and restores the mobile stack', async (_kind, choice) => {
        mocks.alertAddCharacter.mockResolvedValue(choice)
        let installs = 0
        const failure = new Error(`${_kind} publication failed`)
        mocks.commitCharacterAddition.mockImplementation(async (request) => {
            installs++
            request.install()
            throw failure
        })

        await expect(addCharacter()).resolves.toBeUndefined()

        expect(installs).toBe(1)
        expect(mocks.database.characters).toHaveLength(1)
        expect(mocks.activateCharacter).not.toHaveBeenCalled()
        expect(mocks.alertError).toHaveBeenCalledOnce()
        expect(mocks.alertError).toHaveBeenCalledWith(failure)
        expect(get(MobileGUIStack)).toBe(1)
    })

    it('assigns an ID when formatting creates an empty-chat fallback', () => {
        const character = createBlankChar()
        character.chats = []

        const formatted = characterFormatUpdate(character)

        expect(formatted.chats[0].id).toBeTruthy()
    })
})

describe('character activation retry', () => {
    beforeEach(() => {
        mocks.database.characters = []
        mocks.nextId = 0
        mocks.navigationGeneration = 0
        OpenRealmStore.set(false)
        vi.clearAllMocks()
        mocks.deactivateActiveWorkingSet.mockResolvedValue(true)
        mocks.activateCharacter.mockReset()
        mocks.activateCharacter.mockImplementation(async () => {
            mocks.navigationGeneration++
            return true
        })
        mocks.readPersistentCharacterDetail.mockImplementation(async (id) => {
            const character = mocks.database.characters.find((candidate) => candidate.chaId === id)
            if (!character) return null
            const { chats: _chats, ...detail } = structuredClone(character)
            return detail
        })
    })

    it('deactivates the working set before opening the Realm catalog', async () => {
        mocks.alertAddCharacter.mockResolvedValue('importFromRealm')

        await addCharacter()

        expect(mocks.deactivateActiveWorkingSet).toHaveBeenCalledOnce()
    })

    it('keeps the current destination when Realm deactivation fails', async () => {
        mocks.alertAddCharacter.mockResolvedValue('importFromRealm')
        mocks.deactivateActiveWorkingSet.mockResolvedValueOnce(false)

        await addCharacter()

        expect(get(OpenRealmStore)).toBe(false)
    })

    it('does not retry an activation superseded by newer character navigation', async () => {
        const first = createBlankChar()
        const second = createBlankChar()
        mocks.database.characters.push(first, second)
        const firstActivation = deferred<boolean>()
        mocks.activateCharacter.mockImplementation(async (id) => {
            mocks.navigationGeneration++
            if (id === first.chaId) return firstActivation.promise
            return true
        })

        const older = changeChar(0)
        await vi.waitFor(() =>
            expect(mocks.activateCharacter).toHaveBeenCalledWith(first.chaId, {
                normalize: expect.any(Function),
            }),
        )
        await expect(changeChar(1)).resolves.toBe(true)
        firstActivation.resolve(false)

        await expect(older).resolves.toBe(false)
        expect(mocks.activateCharacter.mock.calls.map(([id]) => id)).toEqual([
            first.chaId,
            second.chaId,
        ])
    })

    it('retries activation once when the first attempt fails', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        const results = [false, true]
        mocks.activateCharacter.mockImplementation(async () => {
            mocks.navigationGeneration++
            return results.shift() ?? false
        })

        const changed = await changeChar(0)

        expect(changed).toBe(true)
        expect(mocks.activateCharacter).toHaveBeenCalledTimes(2)
    })

    it('gives up after exactly one retry', async () => {
        const character = createBlankChar()
        mocks.database.characters.push(character)
        mocks.activateCharacter.mockImplementation(async () => {
            mocks.navigationGeneration++
            return false
        })

        const changed = await changeChar(0)

        expect(changed).toBe(false)
        expect(mocks.activateCharacter).toHaveBeenCalledTimes(2)
    })
})

describe('chat list operations', () => {
    const buildCharacter = () => {
        const character = createBlankChar()
        character.chats = [
            { message: [], note: '', name: 'Chat A', localLore: [], id: 'chat-a' },
            { message: [], note: '', name: 'Chat B', localLore: [], id: 'chat-b' },
            { message: [], note: '', name: 'Chat C', localLore: [], id: 'chat-c' },
        ]
        character.chatPage = 1
        return character
    }
    const buildSelectedCharacter = () => {
        const character = buildCharacter()
        mocks.database.characters.push(character)
        selectedCharID.set(mocks.database.characters.length - 1)
        return character
    }
    // Mirrors activation moving the selected page to the requested chat.
    const selectChat = async (id?: string | number) => {
        const character = mocks.database.characters[get(selectedCharID)]
        const index = character?.chats.findIndex((chat: any) => chat.id === id) ?? -1
        if (index === -1) return false
        character.chatPage = index
        return true
    }

    beforeEach(() => {
        doingChat.set(false)
        mocks.database.characters = []
        mocks.nextId = 0
        selectedCharID.set(-1)
        vi.clearAllMocks()
        mocks.readPersistentConversation.mockReset()
        mocks.changeChatTo.mockReset()
        mocks.changeChatTo.mockImplementation(selectChat)
        mocks.captureSelectedConversationTarget.mockReset()
        mocks.captureSelectedConversationTarget.mockReturnValue(null)
        mocks.acquireCompleteConversation.mockReset()
        mocks.flushPendingData.mockReset()
        mocks.getSelectedConversationMode.mockReset()
        mocks.getSelectedConversationMode.mockReturnValue(null)
        mocks.editWindowedChatList.mockReset()
    })

    it('refuses list edits during generation before promoting or changing the selected chat', async () => {
        const character = buildSelectedCharacter()
        mocks.captureSelectedConversationTarget.mockReturnValue({
            characterId: character.chaId, conversationId: 'chat-b',
        })
        mocks.getSelectedConversationMode.mockReturnValue('windowed')
        const edit = vi.fn(() => null)
        doingChat.set(true)
        try {
            expect(await editSelectedChatList(character.chaId, 'reorder-chats', edit)).toBe(false)
        } finally {
            doingChat.set(false)
        }
        expect(edit).not.toHaveBeenCalled()
        expect(mocks.flushPendingData).not.toHaveBeenCalled()
        expect(mocks.editWindowedChatList).not.toHaveBeenCalled()
        expect(mocks.acquireCompleteConversation).not.toHaveBeenCalled()
        expect(character.chats.map(chat => chat.id)).toEqual(['chat-a', 'chat-b', 'chat-c'])
    })

    it('refuses the complete fallback when generation starts during the windowed flush', async () => {
        const character = buildSelectedCharacter()
        mocks.captureSelectedConversationTarget.mockReturnValue({
            characterId: character.chaId, conversationId: 'chat-b',
        })
        mocks.getSelectedConversationMode.mockReturnValue('windowed')
        mocks.flushPendingData.mockImplementation(async () => { doingChat.set(true) })
        mocks.editWindowedChatList.mockReturnValue({ kind: 'unsupported' })
        const edit = vi.fn(() => null)
        try {
            expect(await editSelectedChatList(character.chaId, 'reorder-chats', edit)).toBe(false)
        } finally {
            doingChat.set(false)
        }
        expect(edit).not.toHaveBeenCalled()
        expect(mocks.acquireCompleteConversation).not.toHaveBeenCalled()
    })

    it('gives every message of an imported SillyTavern chat its own ID', async () => {
        const character = buildSelectedCharacter()
        character.name = 'Synthetic'
        mocks.editWindowedChatList.mockReturnValue({ kind: 'unsupported' })
        const lines = [
            { user_name: 'User', character_name: 'Synthetic' },
            { name: 'Synthetic', is_user: false, mes: 'greeting' },
            { name: 'User', is_user: true, mes: 'reply' },
        ].map((line) => JSON.stringify(line)).join('NEWLINE').split('NEWLINE').join(String.fromCharCode(10))
        mocks.selectSingleFile.mockResolvedValueOnce({
            name: 'synthetic.jsonl',
            data: new TextEncoder().encode(lines),
        })

        await importChat()

        const imported = character.chats[0]
        expect(imported.name).toBe('Imported Chat')
        expect(imported.message.map((message: any) => message.data)).toEqual(['greeting', 'reply'])
        expect(imported.message.map((message: any) => message.chatId)).toEqual([
            expect.stringMatching(/^generated-/),
            expect.stringMatching(/^generated-/),
        ])
        expect(new Set(imported.message.map((message: any) => message.chatId)).size).toBe(2)
    })

    it('imports a risuChat file without decoding it through Buffer', async () => {
        const character = buildSelectedCharacter()
        mocks.editWindowedChatList.mockReturnValue({ kind: 'unsupported' })
        const message = [
            { role: 'user', data: '"Synthetic line." 한글 🐿️', chatId: 'synthetic-msg-0', time: 1710000000000 },
            { role: 'char', data: '*Synthetic action.* 한글 🐿️', chatId: 'synthetic-msg-1', time: 1710000000001 },
        ]
        mocks.selectSingleFile.mockResolvedValueOnce({
            name: 'synthetic-chat.json',
            data: new TextEncoder().encode(JSON.stringify({
                type: 'risuChat',
                ver: 2,
                data: { id: 'synthetic-source', name: 'Synthetic import', note: '', localLore: [], message },
                folders: [],
            })),
        })
        const bufferFrom = vi.spyOn(Buffer, 'from')
        let bufferCalls: number
        try {
            await importChat()
        } finally {
            bufferCalls = bufferFrom.mock.calls.length
            bufferFrom.mockRestore()
        }

        const imported = character.chats[0]
        expect(imported.name).toBe('Synthetic import')
        expect(imported.id).not.toBe('synthetic-source')
        expect(imported.message).toEqual(message)
        expect(bufferCalls).toBe(0)
    })

    it('imports and immediately duplicates large Unicode chats through bounded capture and commit pages', async () => {
        const { SaveCoordinator } = await import('./storage/saveCoordinator')
        const { createPersistenceCanonicalCapture } = await import('./storage/reactivePersistenceCapture.svelte')
        const { createPersistentSaveObserverHarness } = await import('./storage/tests/persistentSaveObserverHarness.svelte')
        const { cloneConversationByMessage, CONVERSATION_INSERT_PAGE_BYTES } = await import('./storage/conversationInsertPages')
        const { utf8ByteLength } = await import('./storage/nativePersistenceValue')
        const { encodeNativeCommit } = await import('./storage/nativeCommitTransport')
        let encodedRequests = 0
        class EncodingWorker {
            onmessage: ((event: { data: { bytes: Uint8Array } }) => void) | null = null
            onerror: (() => void) | null = null
            terminate() {}
            postMessage(data: { pages: Uint8Array[]; byteLength: number }, transfer: ArrayBuffer[]) {
                expect(Object.keys(data)).toEqual(['pages', 'byteLength'])
                expect(data.byteLength).toBeLessThan(CONVERSATION_INSERT_PAGE_BYTES + 64 * 1024)
                expect(data.pages.every((page) => page.byteLength <= 1024 * 1024)).toBe(true)
                const received = structuredClone(data, { transfer })
                expect(data.pages.every((page) => page.byteLength === 0)).toBe(true)
                const bytes = new Uint8Array(received.byteLength)
                let offset = 0
                for (const page of received.pages) { bytes.set(page, offset); offset += page.byteLength }
                encodedRequests++
                Promise.resolve().then(() => this.onmessage?.({ data: { bytes } }))
            }
        }
        vi.stubGlobal('Worker', EncodingWorker)
        const initial = buildSelectedCharacter()
        const reactive = createPersistentSaveObserverHarness({ ...mocks.database } as any)
        mocks.database.characters = reactive.database.characters
        const current = () => mocks.database.characters[0]
        const stored = new Map(current().chats.map((chat: any) => [chat.id, cloneConversationByMessage(chat)]))
        let revision = 1
        const requests: any[] = []
        const capture = createPersistenceCanonicalCapture({
            root: () => reactive.database, presets: () => [], pluginStorage: () => null,
            character: current, characters: () => mocks.database.characters,
        })
        const store = {
            commit: vi.fn(async (input: any) => {
                expect(input.expectedRevision).toBe(revision)
                const encoded = await encodeNativeCommit({ commit: input, assetAliases: [] })
                expect(JSON.parse(new TextDecoder().decode(encoded)).commit).toEqual(input)
                requests.push(input)
                for (const mutation of input.conversations ?? []) {
                    if (mutation.type !== 'replace-range') continue
                    const previous: any = stored.get(mutation.conversationId)
                    const chat = previous ?? { ...mutation.conversation, message: [] }
                    chat.message.splice(mutation.start, mutation.deleteCount, ...mutation.messages)
                    stored.set(mutation.conversationId, chat)
                }
                return { revision: ++revision }
            }),
            readConversation: vi.fn(async (_characterId: string, id: string) => {
                const chat = stored.get(id)
                return chat ? { revision, value: cloneConversationByMessage(chat as any) } : null
            }),
        }
        const coordinator = new SaveCoordinator({
            store: store as any, canonicalCapture: capture,
            captureRoot: () => ({}) as any, capturePresets: () => [], capturePluginStorage: () => null,
            captureCharacters: () => mocks.database.characters, captureSelectedCharacter: current,
            captureCharacter: (id) => mocks.database.characters.find((value: any) => value.chaId === id) ?? null, replaceDatabase: () => {},
        })
        coordinator.initialize(revision)
        mocks.getSelectedConversationMode.mockReturnValue('complete')
        mocks.captureSelectedConversationTarget.mockImplementation(() => ({
            characterId: current().chaId, conversationId: current().chats[current().chatPage].id,
        }))
        mocks.acquireCompleteConversation.mockResolvedValue({ release: () => {} })
        mocks.flushPendingData.mockImplementation((reason) => coordinator.flushPendingData(reason))
        mocks.commitCharacterAddition.mockImplementation((request, reason) => coordinator.commitCharacterAddition(request, reason))
        mocks.readPersistentConversation.mockImplementation((characterId, id, reason) => coordinator.readPersistentConversation(characterId, id, reason))
        mocks.changeChatTo.mockImplementation(async (id) => {
            const result = await selectChat(id)
            await coordinator.flushPendingData('after-duplicate-selection')
            return result
        })
        const data = '한글🐿️é'.repeat(550_000)
        const messages = [0, 1, 2].map((index) => ({ role: 'user', data: `${index}:${data}`, chatId: `import-message-${index}` }))
        mocks.selectSingleFile.mockResolvedValueOnce({ name: 'large-synthetic.json', data: new TextEncoder().encode(JSON.stringify({
            type: 'risuChat', ver: 2, data: { id: 'input', name: 'Large import', note: '', localLore: [], message: messages }, folders: [],
        })) })
        const originalStringify = JSON.stringify
        let largestEncoding = 0
        const stringify = vi.spyOn(JSON, 'stringify').mockImplementation(((value: unknown, ...args: any[]) => {
            const json = (originalStringify as any)(value, ...args)
            if (typeof json === 'string') largestEncoding = Math.max(largestEncoding, utf8ByteLength(json))
            return json
        }) as typeof JSON.stringify)
        try {
            await importChat()
            expect(mocks.alertError).not.toHaveBeenCalled()
            const imported = current().chats[0].id
            expect(await duplicateChat(initial.chaId, imported)).toBe(true)
            const firstDuplicate = current().chats[0].id
            expect(await duplicateChat(initial.chaId, firstDuplicate)).toBe(true)
            const secondDuplicate = current().chats[0].id
            for (const id of [imported, firstDuplicate, secondDuplicate]) {
                expect((stored.get(id) as any).message.map((message: any) => message.data)).toEqual(messages.map((message) => message.data))
            }
            await commitDetachedCharacter({ type: 'character', chaId: 'new-character', name: 'New character',
                chats: [{ id: 'new-character-chat', name: 'New chat', note: '', localLore: [], message: messages }] } as any, 'large-character-import')
            expect((stored.get('new-character-chat') as any).message.map((message: any) => message.data)).toEqual(messages.map((message) => message.data))
            expect(requests.find((request) => request.addCharacter)?.addCharacter.chats).toEqual([])
            expect(largestEncoding).toBeLessThan(CONVERSATION_INSERT_PAGE_BYTES + 64 * 1024)
            expect(requests.filter((request) => request.conversations?.some((mutation: any) => mutation.start > 0))).toHaveLength(4)
            expect(encodedRequests).toBe(requests.length)
            expect(coordinator.hasPendingPersistenceWork).toBe(false)
        } finally {
            stringify.mockRestore()
            vi.unstubAllGlobals()
            mocks.commitCharacterAddition.mockImplementation(async (request: any) => request.install())
        }
    }, 30_000)

    it('keeps the selected chat when another chat is removed', async () => {
        const character = buildSelectedCharacter()

        const removed = await removeChat(character, 'chat-a')

        expect(removed).toBe(true)
        expect(character.chats.map((chat: any) => chat.id)).toEqual(['chat-b', 'chat-c'])
        expect(character.chatPage).toBe(0)
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('moves to a surviving chat before removing the selected chat', async () => {
        const character = buildSelectedCharacter()

        await removeChat(character, 'chat-b')

        expect(mocks.changeChatTo).toHaveBeenCalledWith('chat-a')
        expect(character.chats.map((chat: any) => chat.id)).toEqual(['chat-a', 'chat-c'])
        expect(character.chatPage).toBe(0)
    })

    it('keeps the selected chat when activation of the survivor fails twice', async () => {
        const character = buildSelectedCharacter()
        character.chatPage = 2
        mocks.changeChatTo.mockResolvedValue(false)

        expect(await removeChat(character, 'chat-c')).toBe(false)

        expect(character.chats.map((chat: any) => chat.id)).toEqual(['chat-a', 'chat-b', 'chat-c'])
        expect(character.chatPage).toBe(2)
        expect(mocks.changeChatTo).toHaveBeenCalledTimes(2)
    })

    it('retries activation once before removing the selected chat', async () => {
        const character = buildSelectedCharacter()
        mocks.changeChatTo.mockResolvedValueOnce(false).mockImplementationOnce(selectChat)

        expect(await removeChat(character, 'chat-b')).toBe(true)

        expect(mocks.changeChatTo).toHaveBeenCalledTimes(2)
        expect(mocks.changeChatTo).toHaveBeenNthCalledWith(2, 'chat-a')
        expect(character.chats.map((chat: any) => chat.id)).toEqual(['chat-a', 'chat-c'])
    })

    it('holds the selected conversation complete while the chat list changes', async () => {
        const character = buildSelectedCharacter()
        const target = { characterId: character.chaId, conversationId: 'chat-b' }
        const events: string[] = []
        const release = vi.fn(() => events.push('release'))
        mocks.captureSelectedConversationTarget.mockReturnValue(target)
        mocks.acquireCompleteConversation.mockImplementation(async () => {
            events.push('acquire')
            return { release }
        })
        mocks.flushPendingData.mockImplementation(async () => {
            events.push(`flush:${character.chats[character.chatPage].id}`)
        })
        mocks.changeChatTo.mockImplementation(async (id?: string | number) => {
            events.push(`navigate:${id}`)
            return selectChat(id)
        })

        expect(await addNewChat(character)).toBe(true)

        expect(mocks.acquireCompleteConversation).toHaveBeenCalledWith('add-chat', target)
        expect(events).toEqual(['acquire', 'flush:chat-b', `navigate:${character.chats[0].id}`, 'release'])
        expect(character.chatPage).toBe(0)
    })

    it('edits a windowed selection without holding the conversation complete', async () => {
        const character = buildSelectedCharacter()
        const target = { characterId: character.chaId, conversationId: 'chat-b' }
        const events: string[] = []
        mocks.captureSelectedConversationTarget.mockReturnValue(target)
        mocks.getSelectedConversationMode.mockReturnValue('windowed')
        mocks.flushPendingData.mockImplementation(async () => {
            events.push(`flush:${character.chats.length}`)
        })
        mocks.editWindowedChatList.mockImplementation((captured, edit) => {
            events.push('edit')
            expect(captured).toBe(target)
            const nextId = edit(character)
            return { kind: 'applied', nextId }
        })
        mocks.changeChatTo.mockImplementation(async (id?: string | number) => {
            events.push(`navigate:${id}`)
            return selectChat(id)
        })

        expect(await addNewChat(character)).toBe(true)

        expect(mocks.acquireCompleteConversation).not.toHaveBeenCalled()
        expect(events).toEqual(['flush:3', 'edit', 'flush:4', `navigate:${character.chats[0].id}`])
        expect(mocks.flushPendingData).toHaveBeenCalledWith('add-chat')
    })

    it('does not save or navigate when the windowed edit is refused', async () => {
        const character = buildSelectedCharacter()
        mocks.captureSelectedConversationTarget.mockReturnValue({
            characterId: character.chaId,
            conversationId: 'chat-b',
        })
        mocks.getSelectedConversationMode.mockReturnValue('windowed')
        mocks.editWindowedChatList.mockReturnValue({ kind: 'refused' })

        expect(await addNewChat(character)).toBe(false)

        expect(mocks.flushPendingData).toHaveBeenCalledTimes(1)
        expect(mocks.acquireCompleteConversation).not.toHaveBeenCalled()
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('falls back to complete ownership when the windowed edit cannot be described', async () => {
        const character = buildSelectedCharacter()
        const target = { characterId: character.chaId, conversationId: 'chat-b' }
        const release = vi.fn()
        mocks.captureSelectedConversationTarget.mockReturnValue(target)
        mocks.getSelectedConversationMode.mockReturnValue('windowed')
        mocks.editWindowedChatList.mockReturnValue({ kind: 'unsupported' })
        mocks.acquireCompleteConversation.mockResolvedValue({ release })

        expect(await addNewChat(character)).toBe(true)

        expect(mocks.acquireCompleteConversation).toHaveBeenCalledWith('add-chat', target)
        expect(character.chats).toHaveLength(4)
        expect(character.chats[character.chatPage].id).toBe(character.chats[0].id)
        expect(release).toHaveBeenCalledTimes(1)
    })

    it('does not change the chat list when complete ownership is stale', async () => {
        const character = buildSelectedCharacter()
        mocks.captureSelectedConversationTarget.mockReturnValue({
            characterId: character.chaId,
            conversationId: 'chat-b',
        })
        mocks.acquireCompleteConversation.mockRejectedValue(new SelectedConversationPromotionStaleError())

        expect(await addNewChat(character)).toBe(false)

        expect(character.chats).toHaveLength(3)
        expect(character.chatPage).toBe(1)
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('does not change the chat list while another character owns the selected conversation', async () => {
        const character = buildSelectedCharacter()
        mocks.captureSelectedConversationTarget.mockReturnValue({
            characterId: 'previous-character',
            conversationId: 'previous-chat',
        })

        expect(await addNewChat(character)).toBe(false)

        expect(character.chats).toHaveLength(3)
        expect(mocks.acquireCompleteConversation).not.toHaveBeenCalled()
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('keeps the selected page on the same chat when chats are inserted in front', async () => {
        const character = buildSelectedCharacter()

        expect(await editSelectedChatList(character.chaId, 'import-chat', (current) => {
            current.chats.unshift({ message: [], note: '', name: 'Imported', localLore: [], id: 'imported' })
            return null
        })).toBe(true)

        expect(character.chatPage).toBe(2)
        expect(character.chats[character.chatPage].id).toBe('chat-b')
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('does nothing for an unknown chat id', async () => {
        const character = buildCharacter()

        const removed = await removeChat(character, 'missing')

        expect(removed).toBe(false)
        expect(character.chats).toHaveLength(3)
        expect(character.chatPage).toBe(1)
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('adds a new chat in front and activates it', async () => {
        const character = buildSelectedCharacter()

        const added = await addNewChat(character)

        expect(added).toBe(true)
        expect(character.chats).toHaveLength(4)
        expect(character.chats[0].name).toBe('New Chat 4')
        expect(character.chats[0].id).toBeTruthy()
        expect(mocks.changeChatTo).toHaveBeenCalledWith(character.chats[0].id)
    })

    it('does not apply semantic format migrations to conversation summary stubs', () => {
        const character = buildCharacter()
        character.firstMsgIndex = 2
        character.chats[0] = createConversationSummaryStub({
            id: 'chat-a',
            characterId: character.chaId,
            name: 'Chat A',
            configuredIndex: 0,
            recentAt: 0,
            messageCount: 3,
        })

        characterFormatUpdate(character)

        expect(character.chats[0]).not.toHaveProperty('fmIndex')
    })

    it('normalizes character and chat metadata without reading a metadata-only message array', () => {
        const character = buildCharacter()
        character.type = undefined as any
        character.customscript = undefined as any
        const shell = createMetadataOnlySelectedConversation(character.chats[0])
        delete shell.fmIndex
        delete shell.localLore
        character.chats[0] = shell
        character.chatPage = 0

        const formatted = characterFormatUpdate(character)

        expect(formatted.type).toBe('character')
        expect(formatted.customscript).toEqual([])
        expect(formatted.chats[0]).toBe(shell)
        expect(formatted.chats[0].fmIndex).toBe(-1)
        expect(formatted.chats[0].localLore).toEqual([])
    })

    it('reads a chat authoritatively and navigates only to its duplicate', async () => {
        const character = buildCharacter()
        const authoritative = {
            ...character.chats[0],
            message: [{ role: 'user', data: 'authoritative body' }],
        }
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.readPersistentConversation.mockResolvedValue(authoritative)

        const duplicated = await duplicateChat(character.chaId, 'chat-a')

        expect(duplicated).toBe(true)
        expect(mocks.readPersistentConversation).toHaveBeenCalledWith(
            character.chaId,
            'chat-a',
            'duplicate-chat',
        )
        expect(character.chats[0]).toMatchObject({
            id: expect.stringMatching(/^generated-/),
            name: 'Chat A (Copy)',
            message: [{ role: 'user', data: 'authoritative body' }],
        })
        expect(mocks.changeChatTo).toHaveBeenCalledTimes(1)
        expect(mocks.changeChatTo).toHaveBeenCalledWith(character.chats[0].id)
    })

    it('does not install a duplicate after the selected character changes', async () => {
        const sourceCharacter = buildCharacter()
        const nextCharacter = createBlankChar()
        const nextChatIds = nextCharacter.chats.map((chat: any) => chat.id)
        const read = deferred<any>()
        mocks.database.characters.push(sourceCharacter, nextCharacter)
        selectedCharID.set(0)
        mocks.readPersistentConversation.mockReturnValue(read.promise)

        const pending = duplicateChat(sourceCharacter.chaId, 'chat-a')
        selectedCharID.set(1)
        read.resolve(structuredClone(sourceCharacter.chats[0]))

        expect(await pending).toBe(false)
        expect(sourceCharacter.chats).toHaveLength(3)
        expect(nextCharacter.chats.map((chat: any) => chat.id)).toEqual(nextChatIds)
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('does not duplicate a missing authoritative chat', async () => {
        const character = buildCharacter()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.readPersistentConversation.mockResolvedValue(null)

        expect(await duplicateChat(character.chaId, 'chat-a')).toBe(false)
        expect(character.chats).toHaveLength(3)
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('does not restore a source chat deleted while the read was pending', async () => {
        const character = buildCharacter()
        const read = deferred<any>()
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.readPersistentConversation.mockReturnValue(read.promise)

        const pending = duplicateChat(character.chaId, 'chat-a')
        character.chats.splice(0, 1)
        read.resolve({ message: [], note: '', name: 'Chat A', localLore: [], id: 'chat-a' })

        expect(await pending).toBe(false)
        expect(character.chats.map((chat: any) => chat.id)).toEqual(['chat-b', 'chat-c'])
        expect(mocks.changeChatTo).not.toHaveBeenCalled()
    })

    it('reads a nonselected chat authoritatively before export', async () => {
        const character = buildCharacter()
        character.chats[0].message = []
        mocks.database.characters.push(character)
        selectedCharID.set(0)
        mocks.readPersistentConversation.mockResolvedValue({
            ...character.chats[0],
            message: [{ role: 'user', data: 'authoritative body' }],
        })

        await exportChat(0)

        expect(mocks.readPersistentConversation).toHaveBeenCalledWith(
            character.chaId,
            'chat-a',
            'export-chat',
        )
        const exported = JSON.parse(Buffer.from(mocks.downloadFile.mock.calls[0][1]).toString())
        expect(exported.data.message).toEqual([
            { role: 'user', data: 'authoritative body' },
        ])
    })

    it('reads the complete character authoritatively before exporting all chats', async () => {
        const resident = buildCharacter()
        resident.chats[0].message = []
        const complete = structuredClone(resident)
        complete.chats[0].message = [{ role: 'user', data: 'authoritative body' }]
        mocks.database.characters.push(resident)
        selectedCharID.set(0)
        mocks.readPersistentCompleteCharacter.mockResolvedValue(complete)

        await exportAllChats()

        expect(mocks.readPersistentCompleteCharacter).toHaveBeenCalledWith(
            resident.chaId,
            'export-all-chats',
        )
        const exported = JSON.parse(Buffer.from(mocks.downloadFile.mock.calls[0][1]).toString())
        expect(exported.data[0].message).toEqual([
            { role: 'user', data: 'authoritative body' },
        ])
    })
})
