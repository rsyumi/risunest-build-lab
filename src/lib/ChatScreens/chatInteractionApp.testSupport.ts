import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { vi } from 'vitest'
import type { Database, Message, character } from 'src/ts/storage/database.svelte'

export const interactionCharacterId = 'synthetic-interaction-owner'
export const interactionConversationId = 'synthetic-interaction-chat'

export async function bootChatInteractionApp() {
    vi.resetModules()
    // Each boot needs the probe compiled against its own Svelte module graph.
    vi.doMock('./Chat.svelte', async () => ({ default: (await import('./ChatMountProbe.test.svelte')).default }))
    // Vite evaluates cyclic imports asynchronously. Flush root effects only once
    // the graph is initialized, as in the synchronously bundled application.
    vi.useFakeTimers({ toFake: ['queueMicrotask'] })
    const indexedDB = new IDBFactory()
    const previousGlobals = { indexedDB: globalThis.indexedDB, IDBKeyRange: globalThis.IDBKeyRange }
    Object.assign(globalThis, { indexedDB, IDBKeyRange })
    const stores = await import('src/ts/stores.svelte')
    const database = await import('src/ts/storage/database.svelte')
    const runtimeModule = await import('src/ts/storage/persistentDataRuntime.svelte')
    const factory = await import('src/ts/storage/persistentDataStoreFactory')
    const preparation = await import('src/ts/storage/databasePreparation')
    const catalog = await import('src/ts/storage/workingSetCatalog')
    const { bootstrapPersistentDatabase } = await import('src/ts/storage/persistentBootstrap')
    const { workingSetResidency } = await import('src/ts/storage/workingSetResidency')
    const characters = await import('src/ts/characters')
    const svelte = await import('svelte')
    const { chatMountProbe, resetChatMountProbe } = await import('./chatMountProbe.testSupport')
    vi.runAllTicks()
    vi.useRealTimers()
    resetChatMountProbe()

    const messages: Message[] = Array.from({ length: 5000 }, (_, index) => ({
        role: index % 2 ? 'char' : 'user', data: `Synthetic interaction row ${index}`, chatId: `interaction-${index}`,
    }))
    const owner = {
        type: 'character', chaId: interactionCharacterId, name: 'Synthetic interaction owner', image: '',
        firstMessage: '', firstMsgIndex: -1, desc: '', notes: '', chatPage: 0, viewScreen: 'none', bias: [],
        emotionImages: [], additionalAssets: [], globalLore: [], customscript: [], triggerscript: [],
        chatFolders: [{ id: 'folder-a', name: 'Folder A', folded: false }, { id: 'folder-b', name: 'Folder B', folded: false }],
        chats: [{ id: interactionConversationId, name: 'Synthetic selected chat', folderId: 'folder-a',
            note: '', localLore: [], bookmarks: ['interaction-123'], bookmarkNames: {}, message: messages }],
    } as unknown as character
    const raw = factory.getRawPersistentDataStore()
    await raw.open()
    const prepared = await preparation.prepareDatabaseForBootstrap({
        characters: [owner], characterOrder: [interactionCharacterId], streamingDisplayOptimizationMode: 'balanced',
    } as unknown as Database)
    await raw.replaceFromDatabase(prepared.database, 0)
    const runtime = runtimeModule.getPersistentDataRuntime()
    const local = await bootstrapPersistentDatabase({
        store: runtime.store, prepareDatabase: preparation.prepareDatabaseForBootstrap,
        prepareRoot: preparation.preparePersistentRootForWorkingSet,
        projectScalableWorkingSet: (input) => catalog.projectCatalogWorkingSet(input.root, input.characters,
            catalog.createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset)),
    })
    runtimeModule.configurePersistentDataRuntime({
        projectWorkingSet(value, selectedId, conversationId, activeIds, force) {
            if (force === false) return value
            const projected = catalog.isCatalogPresetWorkingSet(value.botPresets) ? value
                : catalog.projectCompleteScalableWorkingSet(value, selectedId, runtime.revision, activeIds, conversationId)
            for (const item of projected.characters) {
                if (catalog.isWorkingSetCharacterStub(item)) workingSetResidency.markCharacterReleased(item.chaId)
                else workingSetResidency.reconcileConversationResidency(item)
            }
            return projected
        },
    })
    workingSetResidency.clear()
    workingSetResidency.setEvictionAllowed(true)
    for (const item of local.database.characters) workingSetResidency.markCharacterReleased(item.chaId)
    database.setDatabase(local.database)
    stores.selectedCharID.set(-1)
    await runtimeModule.initializeActiveWorkingSet(database.getDatabase())
    await characters.changeChar(0)
    await runtime.flushPendingData('interaction-activation')
    stores.botMakerMode.set(false)
    stores.settingsOpen.set(false)
    stores.MobileSideBar.set(0)
    stores.bookmarkListOpen.set(false)

    const fullReads: string[] = []
    const readConversation = runtime.store.readConversation.bind(runtime.store)
    vi.spyOn(runtime.store, 'readConversation').mockImplementation((characterId, conversationId) => {
        fullReads.push(`${characterId}/${conversationId}`)
        return readConversation(characterId, conversationId)
    })
    const acquireRevision = runtime.store.acquireRevision.bind(runtime.store)
    vi.spyOn(runtime.store, 'acquireRevision').mockImplementation(async (revision) => {
        const lease = await acquireRevision(revision)
        const read = lease.readConversation.bind(lease)
        return Object.assign(Object.create(lease), {
            readConversation: (characterId: string, conversationId: string) => {
                fullReads.push(`${characterId}/${conversationId}`)
                return read(characterId, conversationId)
            },
        })
    })
    const completeLeases = vi.spyOn(runtime, 'acquireCompleteConversation')
    return { runtime, raw, stores, database, characters, svelte, fullReads, completeLeases, chatMountProbe,
        selected: () => database.getDatabase().characters[0] as character,
        restore() {
            workingSetResidency.clear()
            stores.selectedCharID.set(-1)
            Object.assign(globalThis, previousGlobals)
        },
    }
}
