import { vi } from 'vitest'
import type { FileHandle } from '@tauri-apps/plugin-fs'
import type { WebviewWindow } from '@tauri-apps/api/webviewWindow'

// Shared vi.mock module factories that cut the import graph of globalApi.svelte.ts for its tests.
// A test passes the pieces it drives from its own hoisted state. Consume from a test file as:
//   vi.mock('./util', async () => (await import('./globalApi.testSupport')).utilModule())

type Stores = typeof import('./stores.svelte')
type Database = typeof import('./storage/database.svelte')

export function platformBlobStoreModule(resolveBlobStore: typeof import('./storage/platformBlobStore')['resolveBlobStore']) {
    return {
        configureBlobStoreStorageProvider: vi.fn(),
        readBlobForFacade: vi.fn(),
        resolveBlobStore,
        subscribeNativeMediaEndpointChanges: vi.fn(() => vi.fn()),
    } satisfies Partial<typeof import('./storage/platformBlobStore')>
}

export function autoStorageModule() {
    class AutoStorage {
        isAccount = false
        realStorage = {}
        async Init() { /* no-op */ }
        async getItem() { return null }
        async setItem() { /* no-op */ }
        async removeItem() { /* no-op */ }
        async keys() { return [] }
    }
    return {
        AutoStorage: AutoStorage as unknown as typeof import('./storage/autoStorage')['AutoStorage'],
    } satisfies Partial<typeof import('./storage/autoStorage')>
}

export function characterCardsModule() {
    return {
        hubURL: 'https://hub.example',
        // `/rs/` is a Realm-classified path (scripts/realmBlocklist.mjs), so the
        // account asset route must build it from realmHubURL, not hubURL.
        realmHubURL: 'https://realm-hub.example',
        characterURLImport: vi.fn(),
    } satisfies Partial<typeof import('./characterCards')>
}

export function utilModule() {
    return { changeFullscreen: vi.fn(), sleep: async () => undefined } satisfies Partial<typeof import('./util')>
}

export function databaseModule(getDatabase: () => object) {
    return {
        getDatabase: getDatabase as Database['getDatabase'],
        getCurrentCharacter: () => ({ chats: [], chatPage: 0 }) as unknown as ReturnType<Database['getCurrentCharacter']>,
        defaultSdDataFunc: () => [],
        appVer: '0.0.0',
        appSubVer: '',
    } satisfies Partial<Database>
}

export function storesModule(db: object) {
    return {
        MobileGUI: { set: vi.fn() } as unknown as Stores['MobileGUI'],
        botMakerMode: { set: vi.fn() } as unknown as Stores['botMakerMode'],
        selectedCharID: { set: vi.fn() } as unknown as Stores['selectedCharID'],
        loadedStore: { set: vi.fn() } as unknown as Stores['loadedStore'],
        DBState: { db } as unknown as Stores['DBState'],
        LoadingStatusState: {} as Stores['LoadingStatusState'],
        selIdState: { selId: 0 } as Stores['selIdState'],
        ReloadGUIPointer: { set: vi.fn() } as unknown as Stores['ReloadGUIPointer'],
        bodyIntercepterStore: [],
    } satisfies Partial<Stores>
}

export function alertModule() {
    return {
        alertConfirm: vi.fn(),
        alertError: vi.fn(),
        alertMd: vi.fn(),
        alertNormal: vi.fn(),
        alertNormalWait: vi.fn(),
        alertSelect: vi.fn(),
        alertToast: vi.fn(),
        alertTOS: vi.fn(),
        alertRisuServiceTOS: vi.fn(),
        waitAlert: vi.fn(),
    } satisfies Partial<typeof import('./alert')>
}

export function persistentDataRuntimeModule(overrides: Partial<typeof import('./storage/persistentDataRuntime.svelte')> = {}) {
    return {
        activateConversation: vi.fn(),
        captureSelectedConversationTarget: vi.fn(),
        fencePersistentNavigation: vi.fn(),
        configurePersistentDataRuntime: vi.fn(),
        markPersistentDataDirty: vi.fn(),
        replacePersistentDatabase: vi.fn(),
        ...overrides,
    } satisfies Partial<typeof import('./storage/persistentDataRuntime.svelte')>
}

export function persistentSaveNotificationsModule() {
    type Notifications = typeof import('./storage/persistentSaveNotifications')
    return {
        createPersistentSaveObserverInstallation: () => ({ install: vi.fn() }) as unknown as ReturnType<Notifications['createPersistentSaveObserverInstallation']>,
        installPersistentSaveNotifications: vi.fn(),
    } satisfies Partial<Notifications>
}

export function databasePreparationModule() {
    return { checkCharOrder: vi.fn() } satisfies Partial<typeof import('./storage/databasePreparation')>
}

export function risuSaveModule() {
    return { decodeRisuSave: vi.fn() } satisfies Partial<typeof import('./storage/risuSave')>
}

export function defaultPromptsModule() {
    return {
        defaultJailbreak: '', defaultMainPrompt: '', oldJailbreak: '', oldMainPrompt: '',
    } satisfies { [K in keyof typeof import('./storage/defaultPrompts')]?: string }
}

export function modulesModule() {
    return { moduleUpdate: vi.fn() } satisfies Partial<typeof import('./process/modules')>
}

export function coldStorageDataModule() {
    return {
        listCharacterResources: () => [],
        listDatabaseRootResources: () => [],
        replaceCharacterResources: vi.fn(),
        replaceDatabaseRootResources: vi.fn(),
    } satisfies Partial<typeof import('./process/coldstorageData')>
}

export function pluginsModule() {
    return { loadPlugins: vi.fn() } satisfies Partial<typeof import('./plugins/plugins.svelte')>
}

export function accounterModule() {
    return { loadRisuAccountData: vi.fn() } satisfies Partial<typeof import('./drive/accounter')>
}

export function observerModule() {
    return { startObserveDom: vi.fn() } satisfies Partial<typeof import('./observer.svelte')>
}

export function charactersModule() {
    return { updateLorebooks: vi.fn() } satisfies Partial<typeof import('./characters')>
}

export function hotkeyModule() {
    return { initMobileGesture: vi.fn() } satisfies Partial<typeof import('./hotkey')>
}

export function animationModule() {
    return { updateAnimationSpeed: vi.fn() } satisfies Partial<typeof import('./gui/animation')>
}

export function colorschemeModule() {
    return { updateColorScheme: vi.fn(), updateTextThemeAndCSS: vi.fn() } satisfies Partial<typeof import('./gui/colorscheme')>
}

export function guisizeModule() {
    return { updateGuisize: vi.fn() } satisfies Partial<typeof import('./gui/guisize')>
}

/** globalApi.svelte.ts default-imports the CommonJS namespace, so the stand-in sits under `default`. */
export function streamsaverModule() {
    return { default: { createWriteStream: vi.fn() } } satisfies { default: Partial<typeof import('streamsaver')> }
}

/** With `handle`, `open` resolves to a file handle that writes and closes through it. */
export function tauriFsModule(handle?: { write: (data: Uint8Array) => Promise<number>, close: () => Promise<void> }) {
    type Fs = typeof import('@tauri-apps/plugin-fs')
    return {
        ...handle ? { open: vi.fn(async () => handle as unknown as FileHandle) } : {},
        writeFile: vi.fn(),
        readFile: vi.fn(),
        exists: vi.fn(),
        mkdir: vi.fn(),
        readDir: vi.fn(),
        remove: vi.fn(),
        BaseDirectory: { Download: 1, AppData: 2 } as unknown as Fs['BaseDirectory'],
    } satisfies Partial<Fs>
}

export function tauriCoreModule() {
    return { convertFileSrc: (path: string) => `asset://${path}` } satisfies Partial<typeof import('@tauri-apps/api/core')>
}

export function tauriPathModule() {
    return { join: vi.fn(async (...parts: string[]) => parts.join('/')) } satisfies Partial<typeof import('@tauri-apps/api/path')>
}

export function tauriShellModule() {
    return { open: vi.fn() } satisfies Partial<typeof import('@tauri-apps/plugin-shell')>
}

export function tauriDialogModule() {
    return { save: vi.fn() } satisfies Partial<typeof import('@tauri-apps/plugin-dialog')>
}

export function webviewWindowModule() {
    return {
        getCurrentWebviewWindow: vi.fn(() => ({}) as WebviewWindow),
    } satisfies Partial<typeof import('@tauri-apps/api/webviewWindow')>
}

export function tauriHttpModule() {
    return { fetch: vi.fn() } satisfies Partial<typeof import('@tauri-apps/plugin-http')>
}
