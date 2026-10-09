import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import { setRuntimePerformanceProfile } from './runtimePerformanceProfile'

const state = vi.hoisted(() => ({
    isTauri: false,
    isTauriIOS: false,
    exportIOSFile: vi.fn(),
    fileWrite: vi.fn(async (data: Uint8Array) => data.byteLength),
    fileClose: vi.fn(async () => undefined),
    downloadThroughAndroidSaf: vi.fn(async (_name: string, _data: Uint8Array) => true),
    isTauriMobile: false,
    blobStore: null as any,
    database: { characters: [] as any[] },
    activateConversation: vi.fn(async (_id: string) => true),
    captureSelectedConversationTarget: vi.fn((): any => null),
    fencePersistentNavigation: vi.fn(),
    yieldToUi: vi.fn(async () => {}),
}))

vi.mock('./storage/iosFiles', () => ({ downloadIOSFile: vi.fn(), exportIOSFile: state.exportIOSFile }))
vi.mock('./storage/androidSafDownload', () => ({ downloadThroughAndroidSaf: state.downloadThroughAndroidSaf }))
vi.mock('./storage/nativePaths', () => ({ iosStagingPath: async () => '/synthetic/staging/owned', nativeDataPath: async () => '/synthetic/data' }))
vi.mock('./platform', () => ({
    get isTauriIOS() { return state.isTauriIOS },
    get isTauri() {
        return state.isTauri
    },
    get isTauriMobile() {
        return state.isTauriMobile
    },
}))
vi.mock('./storage/platformBlobStore', () => ({
    configureBlobStoreStorageProvider: vi.fn(),
    readBlobForFacade: vi.fn(),
    resolveBlobStore: async () => state.blobStore,
    subscribeNativeMediaEndpointChanges: vi.fn(() => vi.fn()),
}))
vi.mock('./storage/autoStorage', () => ({
    AutoStorage: class {
        isAccount = false
        realStorage = {}
        async Init() { /* no-op */ }
        async getItem() { return null }
        async setItem() { /* no-op */ }
        async removeItem() { /* no-op */ }
        async keys() { return [] }
    },
}))
vi.mock('./characterCards', () => ({
    hubURL: 'https://hub.example',
    // `/rs/` is a Realm-classified path (scripts/realmBlocklist.mjs), so the
    // account asset route must build it from realmHubURL, not hubURL.
    realmHubURL: 'https://realm-hub.example',
    characterURLImport: vi.fn(),
}))
vi.mock('./util', () => ({
    changeFullscreen: vi.fn(),
    sleep: async () => undefined,
}))
vi.mock('./storage/database.svelte', () => ({
    getDatabase: () => ({}),
    getCurrentCharacter: () => ({ chats: [], chatPage: 0 }),
    defaultSdDataFunc: () => [],
    appVer: '0.0.0',
    appSubVer: '',
}))
vi.mock('./stores.svelte', () => ({
    MobileGUI: { set: vi.fn() },
    botMakerMode: { set: vi.fn() },
    selectedCharID: { set: vi.fn() },
    loadedStore: { set: vi.fn() },
    DBState: { db: state.database },
    LoadingStatusState: {},
    selIdState: { selId: 0 },
    ReloadGUIPointer: { set: vi.fn() },
    bodyIntercepterStore: [],
}))
vi.mock('./alert', () => ({
    alertConfirm: vi.fn(),
    alertError: vi.fn(),
    alertMd: vi.fn(),
    alertNormal: vi.fn(),
    alertNormalWait: vi.fn(),
    alertSelect: vi.fn(),
    alertToast: vi.fn(),
    alertTOS: vi.fn(), alertRisuServiceTOS: vi.fn(),
    waitAlert: vi.fn(),
}))
vi.mock('./storage/persistentDataRuntime.svelte', () => ({
    activateConversation: state.activateConversation,
    captureSelectedConversationTarget: state.captureSelectedConversationTarget,
    fencePersistentNavigation: state.fencePersistentNavigation,
    configurePersistentDataRuntime: vi.fn(),
    markPersistentDataDirty: vi.fn(),
    replacePersistentDatabase: vi.fn(),
}))
vi.mock('./ui/yieldToUi', () => ({ yieldToUi: state.yieldToUi }))
vi.mock('./storage/persistentSaveNotifications', () => ({
    createPersistentSaveObserverInstallation: () => ({ install: vi.fn() }),
    installPersistentSaveNotifications: vi.fn(),
}))
vi.mock('./storage/databasePreparation', () => ({ checkCharOrder: vi.fn() }))
vi.mock('./storage/risuSave', () => ({ decodeRisuSave: vi.fn() }))
vi.mock('./storage/defaultPrompts', () => ({
    defaultJailbreak: '', defaultMainPrompt: '', oldJailbreak: '', oldMainPrompt: '',
}))
vi.mock('./process/modules', () => ({ moduleUpdate: vi.fn() }))
vi.mock('./process/coldstorage.svelte', () => ({
    getColdStorageItem: vi.fn(),
    makeColdData: vi.fn(),
}))
vi.mock('./process/coldstorageData', () => ({
    listCharacterResources: () => [],
    listDatabaseRootResources: () => [],
    replaceCharacterResources: vi.fn(),
    replaceDatabaseRootResources: vi.fn(),
}))
vi.mock('./plugins/plugins.svelte', () => ({ loadPlugins: vi.fn() }))
vi.mock('./parser/parser.svelte', () => ({ hasher: vi.fn(async () => 'hashed') }))
vi.mock('./drive/accounter', () => ({ loadRisuAccountData: vi.fn() }))
vi.mock('./update', () => ({ checkRisuUpdate: vi.fn() }))
vi.mock('./observer.svelte', () => ({ startObserveDom: vi.fn() }))
vi.mock('./characters', () => ({ updateLorebooks: vi.fn() }))
vi.mock('./hotkey', () => ({ initMobileGesture: vi.fn() }))
vi.mock('./gui/animation', () => ({ updateAnimationSpeed: vi.fn() }))
vi.mock('./gui/colorscheme', () => ({ updateColorScheme: vi.fn(), updateTextThemeAndCSS: vi.fn() }))
vi.mock('./gui/guisize', () => ({ updateGuisize: vi.fn() }))
vi.mock('src/lang', () => ({ language: {} }))
vi.mock('streamsaver', () => ({ default: { createWriteStream: vi.fn() } }))
vi.mock('@tauri-apps/plugin-fs', () => ({
    open: vi.fn(async () => ({ write: state.fileWrite, close: state.fileClose })),
    writeFile: vi.fn(), readFile: vi.fn(), exists: vi.fn(), mkdir: vi.fn(),
    readDir: vi.fn(), remove: vi.fn(), BaseDirectory: { Download: 1, AppData: 2 },
}))
vi.mock('@tauri-apps/api/core', () => ({ convertFileSrc: (path: string) => `asset://${path}` }))
vi.mock('@tauri-apps/api/path', () => ({ join: vi.fn(async (...parts: string[]) => parts.join('/')) }))
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn() }))
vi.mock('@tauri-apps/plugin-dialog', () => ({ save: vi.fn() }))
vi.mock('@tauri-apps/api/webviewWindow', () => ({ getCurrentWebviewWindow: vi.fn(() => ({})) }))
vi.mock('@tauri-apps/plugin-http', () => ({ fetch: vi.fn() }))

import {
    changeChatTo,
    downloadFile,
    forageStorage,
    getFileSrc,
    getFileImageSource,
    clearNativeAssetSourceCache,
    invalidateAssetSourceCache,
    LocalWriter,
    saveAsset,
    TauriWriter,
} from './globalApi.svelte'
import { open as openFile, remove, writeFile } from '@tauri-apps/plugin-fs'
import { alertToast } from './alert'
import { doingChat } from './process/generationState'
import { save } from '@tauri-apps/plugin-dialog'
import { navigationActivity } from './ui/navigationActivity'
import { get } from 'svelte/store'

function createFakeBlobStore(entries: { [key: string]: { data: Uint8Array, mime: string } }) {
    return {
        read: vi.fn(async (key: string) => entries[key]?.data ?? null),
        stat: vi.fn(async (key: string) => entries[key] ? { mime: entries[key].mime } : null),
        resolveUrl: vi.fn(async (key: string) => entries[key] ? `asset:///data/${key}` : null),
        put: vi.fn(async (key: string, data: Uint8Array, metadata: { mime: string, ext: string }) => {
            entries[key] = { data, mime: metadata.mime || 'application/octet-stream' }
            return metadata
        }),
        list: vi.fn(async () => []),
        remove: vi.fn(async () => undefined),
    }
}

function deferred<T>() {
    let resolve!: (value: T) => void
    let reject!: (reason?: unknown) => void
    const promise = new Promise<T>((resolvePromise, rejectPromise) => {
        resolve = resolvePromise
        reject = rejectPromise
    })
    return { promise, resolve, reject }
}

beforeEach(() => {
    vi.clearAllMocks()
    state.fileWrite.mockReset().mockImplementation(async data => data.byteLength)
    state.fileClose.mockReset().mockResolvedValue(undefined)
    vi.mocked(openFile).mockClear()
    state.isTauriIOS = false
    state.isTauri = false
    state.isTauriMobile = false
    state.database.characters.length = 0
    state.activateConversation.mockResolvedValue(true)
    state.yieldToUi.mockResolvedValue(undefined)
    setRuntimePerformanceProfile('normal')
    state.isTauriIOS = false
    state.isTauri = false
    ;(forageStorage as any).isAccount = false
    vi.spyOn(console, 'error').mockImplementation(() => undefined)
})

describe('conversation navigation', () => {
    beforeEach(() => {
        state.database.characters.push({
            chaId: 'character-a',
            chats: [{ id: 'chat-a' }, { id: 'chat-b' }],
        })
    })

    test('ignores an unknown chat without fencing the active navigation', async () => {
        await expect(changeChatTo('missing-chat')).resolves.toBe(false)

        expect(state.fencePersistentNavigation).not.toHaveBeenCalled()
        expect(state.yieldToUi).not.toHaveBeenCalled()
        expect(get(navigationActivity)).toBeNull()
    })

    test('keeps the open conversation without reloading it', async () => {
        state.database.characters[0].chatPage = 1
        state.captureSelectedConversationTarget.mockReturnValueOnce({
            characterId: 'character-a',
            conversationId: 'chat-b',
        })
        doingChat.set(true)
        try {
            await expect(changeChatTo('chat-b')).resolves.toBe(true)
        } finally {
            doingChat.set(false)
        }

        expect(state.fencePersistentNavigation).not.toHaveBeenCalled()
        expect(state.yieldToUi).not.toHaveBeenCalled()
        expect(state.activateConversation).not.toHaveBeenCalled()
        expect(alertToast).not.toHaveBeenCalled()
        expect(get(navigationActivity)).toBeNull()
    })

    test('activates the selected row when the open conversation differs', async () => {
        state.database.characters[0].chatPage = 0
        state.captureSelectedConversationTarget.mockReturnValueOnce({
            characterId: 'character-a',
            conversationId: 'chat-b',
        })

        await expect(changeChatTo('chat-b')).resolves.toBe(true)

        expect(state.fencePersistentNavigation).toHaveBeenCalledOnce()
        expect(state.activateConversation).toHaveBeenCalledWith('chat-b')
    })

    test('refuses a chat switch during generation without fencing the running response', async () => {
        doingChat.set(true)
        try {
            await expect(changeChatTo('chat-b')).resolves.toBe(false)
        } finally {
            doingChat.set(false)
        }

        expect(state.fencePersistentNavigation).not.toHaveBeenCalled()
        expect(state.activateConversation).not.toHaveBeenCalled()
        expect(alertToast).toHaveBeenCalledOnce()
        expect(get(navigationActivity)).toBeNull()
    })

    test('publishes activity and yields before starting conversation activation', async () => {
        const paint = deferred<void>()
        state.yieldToUi.mockReturnValueOnce(paint.promise)

        const pending = changeChatTo('chat-b')

        expect(get(navigationActivity)?.kind).toBe('conversation')
        expect(state.fencePersistentNavigation).toHaveBeenCalledOnce()
        expect(state.activateConversation).not.toHaveBeenCalled()

        paint.resolve()
        await expect(pending).resolves.toBe(true)
        expect(get(navigationActivity)).toBeNull()
    })

    test('does not activate a captured chat after character selection changes during the UI yield', async () => {
        const paint = deferred<void>()
        state.yieldToUi.mockReturnValueOnce(paint.promise)

        const pending = changeChatTo('chat-b')
        state.database.characters[0] = {
            chaId: 'character-b',
            chats: [{ id: 'chat-b' }],
        }
        paint.resolve()

        await expect(pending).resolves.toBe(false)
        expect(state.activateConversation).not.toHaveBeenCalled()
        expect(get(navigationActivity)).toBeNull()
    })

    test('does not activate after the selected character loses its chat catalog during the UI yield', async () => {
        const paint = deferred<void>()
        state.yieldToUi.mockReturnValueOnce(paint.promise)

        const pending = changeChatTo('chat-b')
        state.database.characters[0].chats = undefined
        paint.resolve()

        await expect(pending).resolves.toBe(false)
        expect(state.activateConversation).not.toHaveBeenCalled()
        expect(get(navigationActivity)).toBeNull()
    })

    test('keeps newer conversation activity visible when an older activation finishes', async () => {
        const firstActivation = deferred<boolean>()
        const secondActivation = deferred<boolean>()
        state.activateConversation.mockImplementation((id) =>
            id === 'chat-a'
                ? firstActivation.promise
                : secondActivation.promise,
        )

        const older = changeChatTo('chat-a')
        await vi.waitFor(() =>
            expect(state.activateConversation).toHaveBeenCalledWith('chat-a'),
        )
        const newer = changeChatTo('chat-b')
        await vi.waitFor(() =>
            expect(state.activateConversation).toHaveBeenCalledWith('chat-b'),
        )

        firstActivation.resolve(true)
        await expect(older).resolves.toBe(false)
        expect(get(navigationActivity)?.kind).toBe('conversation')

        secondActivation.resolve(true)
        await expect(newer).resolves.toBe(true)
        expect(get(navigationActivity)).toBeNull()
    })

    test('clears conversation activity when activation fails', async () => {
        const failure = new Error('activation failed')
        state.activateConversation.mockRejectedValueOnce(failure)

        await expect(changeChatTo('chat-b')).rejects.toBe(failure)
        expect(get(navigationActivity)).toBeNull()
    })
})

afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
})

describe('getFileSrc account route', () => {
    test('serves the local blob store before the hub URL', async () => {
        const bytes = new Uint8Array([1, 2, 3])
        state.blobStore = createFakeBlobStore({ 'assets/acc-local.png': { data: bytes, mime: 'image/png' } })
        ;(forageStorage as any).isAccount = true

        const src = await getFileSrc('assets/acc-local.png')
        expect(src).toBe(`data:image/png;base64,${Buffer.from(bytes).toString('base64')}`)
    })

    test('falls back to the Realm-routed hub URL when the local store misses the key', async () => {
        state.blobStore = createFakeBlobStore({})
        ;(forageStorage as any).isAccount = true

        const src = await getFileSrc('assets/acc-remote.png')
        expect(src).toBe('https://realm-hub.example/rs/assets/acc-remote.png')
    })

    test('serves the resolved file URL on Tauri before the hub URL', async () => {
        state.isTauri = true
        state.blobStore = createFakeBlobStore({ 'assets/acc-tauri.png': { data: new Uint8Array([1]), mime: 'image/png' } })
        ;(forageStorage as any).isAccount = true

        const src = await getFileSrc('assets/acc-tauri.png')
        expect(src).toBe('asset:///data/assets/acc-tauri.png')
    })
})

describe('getFileSrc tauri asset route', () => {
    test('resolves a native asset without initializing AutoStorage', async () => {
        state.isTauri = true
        state.blobStore = createFakeBlobStore({ 'assets/native.png': { data: new Uint8Array([7]), mime: 'image/png' } })
        const init = vi.spyOn(forageStorage, 'Init')

        await expect(getFileSrc('assets/native.png')).resolves.toBe('asset:///data/assets/native.png')
        expect(init).not.toHaveBeenCalled()
    })

    test('memoizes the resolved URL per key and invalidates it on saveAsset', async () => {
        state.isTauri = true
        state.blobStore = createFakeBlobStore({ 'assets/avatar.png': { data: new Uint8Array([7]), mime: 'image/png' } })

        expect(await getFileSrc('assets/avatar.png')).toBe('asset:///data/assets/avatar.png')
        expect(await getFileSrc('assets/avatar.png')).toBe('asset:///data/assets/avatar.png')
        expect(state.blobStore.resolveUrl).toHaveBeenCalledTimes(1)

        await saveAsset(new Uint8Array([8]), 'avatar', 'avatar.png')
        expect(state.blobStore.put).toHaveBeenCalledTimes(1)

        expect(await getFileSrc('assets/avatar.png')).toBe('asset:///data/assets/avatar.png')
        expect(state.blobStore.resolveUrl).toHaveBeenCalledTimes(2)
    })

    test('shares one in-flight native URL lookup for concurrent requests to the same key', async () => {
        state.isTauri = true
        const pending = deferred<string | null>()
        const started = deferred<void>()
        state.blobStore = createFakeBlobStore({
            'assets/concurrent.png': { data: new Uint8Array([7]), mime: 'image/png' },
        })
        state.blobStore.resolveUrl = vi.fn(() => {
            started.resolve()
            return pending.promise
        })

        const first = getFileSrc('assets/concurrent.png')
        const second = getFileSrc('assets/concurrent.png')
        await started.promise

        expect(state.blobStore.resolveUrl).toHaveBeenCalledTimes(1)
        pending.resolve('asset:///data/assets/concurrent.png')
        await expect(Promise.all([first, second])).resolves.toEqual([
            'asset:///data/assets/concurrent.png',
            'asset:///data/assets/concurrent.png',
        ])
    })

    test('does not reuse an in-flight native URL after overwriting the asset', async () => {
        state.isTauri = true
        const stale = deferred<string | null>()
        const started = deferred<void>()
        state.blobStore = createFakeBlobStore({
            'assets/overwritten-avatar.png': {
                data: new Uint8Array([7]),
                mime: 'image/png',
            },
        })
        state.blobStore.resolveUrl = vi
            .fn()
            .mockImplementationOnce(() => {
                started.resolve()
                return stale.promise
            })
            .mockResolvedValueOnce('asset:///data/assets/fresh-avatar.png')

        const beforeSave = getFileSrc('assets/overwritten-avatar.png')
        await started.promise
        await saveAsset(new Uint8Array([8]), 'overwritten-avatar', 'avatar.png')

        stale.resolve('asset:///data/assets/stale-avatar.png')
        await expect(beforeSave).resolves.toBe(
            'asset:///data/assets/stale-avatar.png',
        )
        await expect(getFileSrc('assets/overwritten-avatar.png')).resolves.toBe(
            'asset:///data/assets/fresh-avatar.png',
        )
        expect(state.blobStore.resolveUrl).toHaveBeenCalledTimes(2)
    })

    test('retries a native URL lookup after an in-flight rejection', async () => {
        state.isTauri = true
        state.blobStore = createFakeBlobStore({
            'assets/retry.png': { data: new Uint8Array([7]), mime: 'image/png' },
        })
        state.blobStore.resolveUrl = vi.fn()
            .mockRejectedValueOnce(new Error('synthetic resolver failure'))
            .mockResolvedValueOnce('asset:///data/assets/retry.png')

        await expect(getFileSrc('assets/retry.png')).rejects.toThrow('synthetic resolver failure')
        await expect(getFileSrc('assets/retry.png')).resolves.toBe('asset:///data/assets/retry.png')
        expect(state.blobStore.resolveUrl).toHaveBeenCalledTimes(2)
    })

    test('does not cache a native URL lookup that was invalidated while in flight', async () => {
        state.isTauri = true
        const stale = deferred<string | null>()
        const fresh = deferred<string | null>()
        const bothStarted = deferred<void>()
        let starts = 0
        state.blobStore = createFakeBlobStore({
            'assets/invalidated.png': { data: new Uint8Array([7]), mime: 'image/png' },
        })
        state.blobStore.resolveUrl = vi.fn()
            .mockImplementationOnce(() => {
                if (++starts === 2) bothStarted.resolve()
                return stale.promise
            })
            .mockImplementationOnce(() => {
                if (++starts === 2) bothStarted.resolve()
                return fresh.promise
            })

        const first = getFileSrc('assets/invalidated.png')
        invalidateAssetSourceCache('assets/invalidated.png')
        const second = getFileSrc('assets/invalidated.png')
        await bothStarted.promise

        stale.resolve('asset:///data/assets/invalidated-stale.png')
        await expect(first).resolves.toBe('asset:///data/assets/invalidated-stale.png')
        const third = getFileSrc('assets/invalidated.png')
        expect(state.blobStore.resolveUrl).toHaveBeenCalledTimes(2)

        fresh.resolve('asset:///data/assets/invalidated-fresh.png')
        await expect(Promise.all([second, third])).resolves.toEqual([
            'asset:///data/assets/invalidated-fresh.png',
            'asset:///data/assets/invalidated-fresh.png',
        ])
        expect(state.blobStore.resolveUrl).toHaveBeenCalledTimes(2)
    })

    test('does not cache a missing asset resolution', async () => {
        state.isTauri = true
        state.blobStore = createFakeBlobStore({})

        expect(await getFileSrc('assets/ghost.png')).toBe('')
        state.blobStore = createFakeBlobStore({ 'assets/ghost.png': { data: new Uint8Array([1]), mime: 'image/png' } })
        expect(await getFileSrc('assets/ghost.png')).toBe('asset:///data/assets/ghost.png')
    })

    test('bounds native URL entries without revoking protocol URLs', async () => {
        state.isTauri = true
        const entries = Object.fromEntries(
            Array.from({ length: 257 }, (_, index) => [`assets/native-${index}.png`, {
                data: new Uint8Array([index]), mime: 'image/png',
            }]),
        )
        state.blobStore = createFakeBlobStore(entries)
        const revokeObjectURL = vi.spyOn(URL, 'revokeObjectURL')

        for (let index = 0; index < 257; index++) {
            await getFileSrc(`assets/native-${index}.png`)
        }
        await getFileSrc('assets/native-0.png')

        expect(state.blobStore.resolveUrl).toHaveBeenCalledTimes(258)
        expect(revokeObjectURL).not.toHaveBeenCalled()
    })
})

describe('getFileSrc browser asset route', () => {
    test('caches the finished data URL so a hit skips stat and re-encoding', async () => {
        const bytes = new Uint8Array([9, 8, 7])
        state.blobStore = createFakeBlobStore({ 'assets/web.png': { data: bytes, mime: 'image/png' } })
        const expected = `data:image/png;base64,${Buffer.from(bytes).toString('base64')}`

        expect(await getFileSrc('assets/web.png')).toBe(expected)
        expect(await getFileSrc('assets/web.png')).toBe(expected)
        expect(state.blobStore.read).toHaveBeenCalledTimes(1)
        expect(state.blobStore.stat).toHaveBeenCalledTimes(1)
    })

    test('returns an empty string for a missing asset', async () => {
        state.blobStore = createFakeBlobStore({})
        expect(await getFileSrc('assets/web-missing.png')).toBe('')
    })

    test('clears retained data URLs when switching to the lower low-spec budget', async () => {
        const bytes = new Uint8Array([4, 5, 6])
        state.blobStore = createFakeBlobStore({ 'assets/profile.png': { data: bytes, mime: 'image/png' } })

        await getFileSrc('assets/profile.png')
        await getFileSrc('assets/profile.png')
        expect(state.blobStore.read).toHaveBeenCalledTimes(1)

        setRuntimePerformanceProfile('low-spec')

        await getFileSrc('assets/profile.png')
        expect(state.blobStore.read).toHaveBeenCalledTimes(2)
    })
})

describe('LocalWriter streamed backup entries', () => {
    test('forwards abort to a browser destination', async () => {
        const localWriter = new LocalWriter()
        const abort = vi.fn(async () => undefined)
        localWriter.writer = {
            write: vi.fn(async () => undefined),
            close: vi.fn(async () => undefined),
            abort,
        } as any

        await localWriter.abort()

        expect(abort).toHaveBeenCalledOnce()
    })

    test('discards buffered bytes without deleting a pre-existing Tauri destination', async () => {
        const writer = new TauriWriter('partial.zip')

        await writer.write(new Uint8Array(4 * 1024 * 1024))
        await writer.write(Uint8Array.of(1, 2, 3))
        await writer.abort()

        expect(state.fileWrite).toHaveBeenCalledOnce()
        expect(state.fileClose).toHaveBeenCalledOnce()
        expect(remove).not.toHaveBeenCalled()
    })

    test('does not delete a pre-existing Tauri destination after a failed flush', async () => {
        state.fileWrite.mockRejectedValueOnce(new Error('write failed'))
        const writer = new TauriWriter('failed.zip')

        await expect(writer.write(new Uint8Array(4 * 1024 * 1024))).rejects.toThrow('write failed')
        await writer.abort()

        expect(remove).not.toHaveBeenCalled()
    })

    test('hands an Android download to the picker bridge and reports its cancellation', async () => {
        state.isTauri = true
        state.isTauriMobile = true
        state.downloadThroughAndroidSaf.mockResolvedValueOnce(false)

        await expect(downloadFile('chat:1.txt', 'text')).resolves.toBe(false)

        expect(state.downloadThroughAndroidSaf).toHaveBeenCalledWith('chat_1.txt', new TextEncoder().encode('text'))
        expect(save).not.toHaveBeenCalled()
        expect(writeFile).not.toHaveBeenCalled()
    })

    test('writes the legacy length-prefixed entry without joining payload chunks', async () => {
        const writes: Uint8Array[] = []
        const localWriter = new LocalWriter()
        localWriter.writer = {
            write: vi.fn(async (value: Uint8Array) => void writes.push(value.slice())),
            close: vi.fn(async () => undefined),
        } as any

        await localWriter.writeBackupStream('db', 5, (async function* () {
            yield Uint8Array.of(1, 2)
            yield Uint8Array.of(3, 4, 5)
        })())

        expect(writes).toEqual([
            Uint8Array.of(2, 0, 0, 0, 100, 98, 5, 0, 0, 0),
            Uint8Array.of(1, 2),
            Uint8Array.of(3, 4, 5),
        ])
    })

    test('closes the payload iterator when the destination rejects a chunk', async () => {
        const destinationError = new Error('destination failed')
        let cleaned = false
        let writes = 0
        const localWriter = new LocalWriter()
        localWriter.writer = {
            write: vi.fn(async () => {
                writes += 1
                if (writes === 2) throw destinationError
            }),
            close: vi.fn(async () => undefined),
        } as any

        const payload = (async function* () {
            try {
                yield Uint8Array.of(1, 2)
                yield Uint8Array.of(3)
            } finally {
                cleaned = true
            }
        })()

        await expect(localWriter.writeBackupStream('db', 3, payload)).rejects.toBe(
            destinationError,
        )
        expect(cleaned).toBe(true)
    })
})

describe('iOS LocalWriter completed-file publication', () => {
    beforeEach(() => {
        state.isTauri = true
        state.isTauriIOS = true
        state.exportIOSFile.mockReset().mockResolvedValue({ bytes: 3 })
    })
    test('streams to owned staging and publishes only after closing', async () => {
        const writer = new LocalWriter()
        await writer.init('Card', ['png'], '캐릭터.png')
        await writer.write(Uint8Array.of(1, 2, 3))
        expect(save).not.toHaveBeenCalled()
        expect(state.exportIOSFile).not.toHaveBeenCalled()
        await writer.close()
        expect(openFile).toHaveBeenLastCalledWith('/synthetic/staging/owned/export.bin', { write: true, create: true, truncate: true, append: false })
        expect(state.fileWrite).toHaveBeenCalledExactlyOnceWith(Uint8Array.of(1, 2, 3))
        expect(state.fileClose).toHaveBeenCalledOnce()
        expect(state.exportIOSFile).toHaveBeenCalledExactlyOnceWith({ sourcePath: '/synthetic/staging/owned/export.bin', suggestedName: '캐릭터.png' })
        expect(remove).toHaveBeenCalledExactlyOnceWith('/synthetic/staging/owned', { recursive: true })
        await writer.close()
        expect(state.exportIOSFile).toHaveBeenCalledOnce()
    })
    test.each([new DOMException('cancelled', 'AbortError'), new Error('provider failed')])('cleans staging and propagates publication failure %s', async (error) => {
        const writer = new LocalWriter()
        await writer.init()
        state.exportIOSFile.mockRejectedValueOnce(error)
        await expect(writer.close()).rejects.toBe(error)
        expect(remove).toHaveBeenCalledOnce()
    })
    test('cleans staging on abort without publishing buffered bytes', async () => {
        const writer = new LocalWriter()
        await writer.init()
        await writer.write(Uint8Array.of(1))
        await writer.abort()
        expect(state.exportIOSFile).not.toHaveBeenCalled()
        expect(remove).toHaveBeenCalledOnce()
        await expect(writer.close()).rejects.toThrow('aborted')
    })
    test('retains staging while an open Files picker settles', async () => {
        let settle!: () => void
        state.exportIOSFile.mockImplementation(() => new Promise<void>(resolve => { settle = resolve }))
        const writer = new LocalWriter()
        await writer.init()
        const closing = writer.close()
        await vi.waitFor(() => expect(state.exportIOSFile).toHaveBeenCalledOnce())
        const aborting = writer.abort()
        expect(remove).not.toHaveBeenCalled()
        settle()
        await Promise.all([closing, aborting])
        expect(remove).toHaveBeenCalledOnce()
    })
    test('does not publish after a failed final flush', async () => {
        const writer = new LocalWriter()
        await writer.init()
        await writer.write(Uint8Array.of(1))
        state.fileWrite.mockRejectedValueOnce(new Error('flush failed'))
        await expect(writer.close()).rejects.toThrow('flush failed')
        expect(state.exportIOSFile).not.toHaveBeenCalled()
        expect(remove).toHaveBeenCalledOnce()
    })
})


describe('bounded TauriWriter', () => {
    test('keeps an untouched close separate from an explicit empty write', async () => {
        await new TauriWriter('untouched.bin').close()
        expect(openFile).not.toHaveBeenCalled()
        const writer = new TauriWriter('empty.bin')
        await writer.write(new Uint8Array())
        await writer.close()
        expect(openFile).toHaveBeenCalledExactlyOnceWith('empty.bin', {
            write: true, create: true, truncate: true, append: false,
        })
        expect(state.fileClose).toHaveBeenCalledOnce()
    })

    test.each([false, true])('bounds every buffer and request with a huge input (Android: %s)', async android => {
        state.isTauriMobile = android
        const limit = android ? 64 * 1024 : 4 * 1024 * 1024
        const input = Uint8Array.from({ length: limit * 2 + 71 }, (_, index) => index % 251)
        const inputSlice = vi.spyOn(input, 'slice')
        const chunks: Uint8Array[] = []
        state.fileWrite.mockImplementation(async chunk => {
            expect(chunk.byteLength).toBeLessThanOrEqual(limit)
            expect(chunk.buffer.byteLength).toBe(limit)
            chunks.push(chunk.slice())
            return chunk.byteLength
        })
        const writer = new TauriWriter(android ? 'content://synthetic/export' : '/synthetic/export')
        await writer.write(Uint8Array.of(252, 253))
        await writer.write(input)
        await writer.close()
        const expected = new Uint8Array(input.byteLength + 2)
        expected.set([252, 253])
        expected.set(input, 2)
        expect(Buffer.compare(Buffer.concat(chunks), Buffer.from(expected))).toBe(0)
        expect(inputSlice).not.toHaveBeenCalled()
        expect(chunks.map(chunk => chunk.byteLength)).toEqual([limit, limit, 73])
        expect(openFile).toHaveBeenCalledExactlyOnceWith(writer.path, {
            write: true, create: true, truncate: true, append: false,
        })
        expect(state.fileClose).toHaveBeenCalledOnce()
        await writer.close()
        expect(state.fileClose).toHaveBeenCalledOnce()
        await expect(writer.write(Uint8Array.of(1))).rejects.toThrow('closed')
    })

    test('retries short writes in order and preserves caller-selected append mode', async () => {
        const chunks: Uint8Array[] = []
        state.fileWrite.mockImplementation(async chunk => {
            const length = Math.min(2, chunk.byteLength)
            chunks.push(chunk.slice(0, length))
            return length
        })
        const writer = new TauriWriter('existing.bin')
        writer.firstWrite = false
        await writer.write(Uint8Array.of(1, 2, 3, 4, 5))
        await writer.close()
        expect(Buffer.concat(chunks)).toEqual(Buffer.from([1, 2, 3, 4, 5]))
        expect(openFile).toHaveBeenCalledWith('existing.bin', {
            write: true, create: true, truncate: false, append: true,
        })
    })

    test.each([0, -1, 4, 1.5, NaN])('rejects invalid write length %s without retrying or reopening', async length => {
        state.fileWrite.mockResolvedValueOnce(length)
        const writer = new TauriWriter('existing.bin')
        await writer.write(Uint8Array.of(1, 2, 3))
        await expect(writer.close()).rejects.toThrow('invalid write length')
        await expect(writer.close()).rejects.toThrow('invalid write length')
        await writer.abort()
        expect(state.fileWrite).toHaveBeenCalledOnce()
        expect(state.fileClose).toHaveBeenCalledOnce()
        expect(remove).not.toHaveBeenCalled()
    })

    test('serializes overlapping writes and close without reusing an in-flight buffer', async () => {
        state.isTauriMobile = true
        const gate = deferred<number>()
        const chunks: Uint8Array[] = []
        state.fileWrite.mockImplementationOnce(async chunk => {
            await gate.promise
            chunks.push(chunk.slice())
            return chunk.byteLength
        }).mockImplementation(async chunk => {
            chunks.push(chunk.slice())
            return chunk.byteLength
        })
        const writer = new TauriWriter('ordered.bin')
        const first = writer.write(new Uint8Array(64 * 1024).fill(1))
        await vi.waitFor(() => expect(state.fileWrite).toHaveBeenCalledOnce())
        const second = writer.write(Uint8Array.of(2, 3))
        const closing = writer.close()
        expect(state.fileWrite).toHaveBeenCalledOnce()
        gate.resolve(64 * 1024)
        await Promise.all([first, second, closing])
        expect(chunks).toEqual([new Uint8Array(64 * 1024).fill(1), Uint8Array.of(2, 3)])
        expect(state.fileClose).toHaveBeenCalledOnce()
    })

    test('cancels between bounded requests and closes the active handle without deleting the destination', async () => {
        state.isTauriMobile = true
        const gate = deferred<number>()
        state.fileWrite.mockImplementationOnce(() => gate.promise)
        const writer = new TauriWriter('existing.bin')
        const writing = writer.write(new Uint8Array(3 * 64 * 1024))
        const rejected = expect(writing).rejects.toThrow('aborted')
        await vi.waitFor(() => expect(state.fileWrite).toHaveBeenCalledOnce())
        const aborting = writer.abort()
        expect(state.fileClose).not.toHaveBeenCalled()
        gate.resolve(64 * 1024)
        await rejected
        await aborting
        expect(state.fileWrite).toHaveBeenCalledOnce()
        expect(state.fileClose).toHaveBeenCalledOnce()
        expect(remove).not.toHaveBeenCalled()
        await expect(writer.close()).rejects.toThrow('aborted')
    })

    test('does not open or truncate a destination when aborted before the first flush', async () => {
        const writer = new TauriWriter('existing.bin')
        await writer.write(Uint8Array.of(1, 2))
        await writer.abort()
        await writer.abort()
        expect(openFile).not.toHaveBeenCalled()
        expect(state.fileWrite).not.toHaveBeenCalled()
        expect(remove).not.toHaveBeenCalled()
    })

    test('keeps a write failure terminal and closes once', async () => {
        state.isTauriMobile = true
        const failure = new Error('synthetic destination failure')
        state.fileWrite.mockRejectedValueOnce(failure)
        const writer = new TauriWriter('existing.bin')
        await expect(writer.write(new Uint8Array(64 * 1024))).rejects.toBe(failure)
        await expect(writer.write(Uint8Array.of(1))).rejects.toBe(failure)
        await expect(writer.close()).rejects.toBe(failure)
        await writer.abort()
        expect(openFile).toHaveBeenCalledOnce()
        expect(state.fileClose).toHaveBeenCalledOnce()
        expect(remove).not.toHaveBeenCalled()
    })

    test('preserves open errors without touching or deleting the destination', async () => {
        const failure = new Error('synthetic open failure')
        vi.mocked(openFile).mockRejectedValueOnce(failure)
        const writer = new TauriWriter('existing.bin')
        await writer.write(Uint8Array.of(1))
        await expect(writer.close()).rejects.toBe(failure)
        await writer.abort()
        expect(openFile).toHaveBeenCalledOnce()
        expect(state.fileWrite).not.toHaveBeenCalled()
        expect(state.fileClose).not.toHaveBeenCalled()
        expect(remove).not.toHaveBeenCalled()
    })

    test('does not publish an iOS staging file if closing its handle fails', async () => {
        state.isTauri = true
        state.isTauriIOS = true
        const failure = new Error('synthetic close failure')
        state.fileClose.mockRejectedValueOnce(failure)
        const writer = new LocalWriter()
        await writer.init()
        await writer.write(Uint8Array.of(1))
        await expect(writer.close()).rejects.toBe(failure)
        expect(state.exportIOSFile).not.toHaveBeenCalled()
        expect(remove).toHaveBeenCalledOnce()
    })

    test('writes a backup header separately from the original payload', async () => {
        const payload = new Uint8Array(64 * 1024 + 7)
        const chunks: Uint8Array[] = []
        const writer = new LocalWriter()
        writer.writer = { write: vi.fn(async (chunk: Uint8Array) => { chunks.push(chunk) }) } as any
        await writer.writeBackup('entry.bin', payload)
        expect(chunks).toHaveLength(2)
        expect(chunks[1]).toBe(payload)
        expect(new DataView(chunks[0].buffer).getUint32(4 + 'entry.bin'.length, true)).toBe(payload.byteLength)
    })
})


test('reloads newly persisted image dimensions from the existing native URL cache after calculation', async () => {
    state.isTauri = true
    let known = false
    const resolveImageSource = vi.fn(async () => ({
        url: 'risuasset://synthetic-geometry', contentHash: 'a'.repeat(64),
        metadata: { key: 'assets/geometry.png', kind: 'asset', mime: 'image/png', ext: 'png', name: 'synthetic', size: 1 },
        ...(known ? { width: 640, height: 480 } : {}), recordDimensions: vi.fn(),
    }))
    state.blobStore = { resolveImageSource, resolveUrl: vi.fn() }
    clearNativeAssetSourceCache()
    expect((await getFileImageSource('assets/geometry.png'))!.width).toBeUndefined()
    known = true
    expect((await getFileImageSource('assets/geometry.png'))!.width).toBeUndefined()
    clearNativeAssetSourceCache()
    expect(await getFileImageSource('assets/geometry.png')).toMatchObject({ width: 640, height: 480 })
    expect(resolveImageSource).toHaveBeenCalledTimes(2)
    expect(state.blobStore.resolveUrl).not.toHaveBeenCalled()
})
