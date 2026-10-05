// @vitest-environment happy-dom
import 'fake-indexeddb/auto'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { PersistentDataRuntime } from '../persistentDataRuntime'

const h = vi.hoisted(() => ({
    invoke: vi.fn(),
    runtime: undefined as unknown as PersistentDataRuntime,
    events: new Map<string, () => void>(),
    applyError: undefined as unknown,
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: h.invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async (name: string, handler: () => void) => { h.events.set(name, handler); return () => { h.events.delete(name) } } }))
vi.mock('src/ts/platform', () => ({ isTauri: true }))
vi.mock('src/ts/stores.svelte', () => ({ selectedCharID: { subscribe: (run: (value: number) => void) => { run(-1); return () => {} } } }))
vi.mock('../database.svelte', () => ({ getDatabase: () => ({ characters: [] }) }))
vi.mock('./bindingDialog', () => ({ confirmSyncBindingReplacement: async () => true, confirmPreviousStorageFiles: async () => 'connect', downloadPreviousStorageFiles: async () => {} }))
vi.mock('./bindingLocalData', () => ({ hasLocalBindingData: async () => false, hasLocalSharedBindingData: async () => false }))
vi.mock('../committedWorkingSetContinuation', () => ({ registerCommittedWorkingSetContinuation: vi.fn() }))
vi.mock('../../mobileBackgroundTask', () => ({ runWithMobileBackgroundTask: (_kind: string, operation: (task: object) => Promise<unknown>) => operation({ progress() {}, async dispose() {} }) }))
vi.mock('src/ts/plugins/apiV3/v3.svelte', () => ({ fencePluginExecutionForAuthorityReplacement: async () => {}, invalidatePluginCachesAfterAuthorityReplacement: async () => {}, restartPluginsAfterAuthorityReplacement: async () => {} }))
// The production wrapper is replaced by a runtime over a real store; LWW receives reach native code through the real SQLite store methods.
vi.mock('../persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: () => h.runtime,
    withPausedPersistentWrites: (reason: string, operation: (token: never) => Promise<unknown>) => h.runtime.withPausedPersistentWrites(reason, operation),
    beginActivatedLibraryGuard: (token: never) => h.runtime.beginActivatedLibraryGuard(token),
    refreshActivatedLibraryUnderPause: async () => ({ projection: 'applied' }),
    applyPersistentLwwReceive: (request: never) => h.runtime.applyLwwReceive(request),
    flushPendingDataLocally: (reason: string) => h.runtime.flushPendingData(reason),
}))

import type { Chat, Database, character } from '../database.svelte'
import { IndexedDbPersistentDataStore } from '../indexedDbPersistentDataStore'
import { capturePersistentRoot, createPersistentDataRuntime, type PersistentDataRuntimeStateAdapter } from '../persistentDataRuntime'
import { notifyLocalPersistentRevision } from '../persistentRevisionEvents'
import { SqlitePersistentDataStore } from '../sqlitePersistentDataStore'

let production: typeof import('./serverSyncProduction')
let workingCopy: Database
const binding = { target: { kind: 'server', connectionId: 'server' }, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
const pulls = () => h.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_pull').length
const settle = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); await new Promise(resolve => setTimeout(resolve, 0)) }

function conversation(messages: number): Chat {
    return {
        id: 'chat-a', name: 'Selected', note: '', localLore: [],
        message: Array.from({ length: messages }, (_, index) => ({ role: index % 2 === 0 ? 'user' as const : 'char' as const, data: `message-${index}`, chatId: `message-${index}` })),
    }
}

async function createRuntime() {
    const database = { username: 'Runtime fixture', characters: [{ type: 'character', chaId: 'char-a', name: 'Alpha', chatPage: 0, chats: [conversation(10_000)] } as unknown as character] } as unknown as Database
    workingCopy = structuredClone(database)
    const store = new IndexedDbPersistentDataStore(`server-sync-runtime-${crypto.randomUUID()}`, indexedDB, IDBKeyRange)
    await store.open()
    await store.replaceFromDatabase(database)
    const sqlite = new SqlitePersistentDataStore()
    Object.assign(store, {
        lwwStageReceive: sqlite.lwwStageReceive.bind(sqlite),
        lwwApplyReceive: sqlite.lwwApplyReceive.bind(sqlite),
        lwwFinishReceive: sqlite.lwwFinishReceive.bind(sqlite),
    })
    const state: PersistentDataRuntimeStateAdapter = {
        captureRoot: () => capturePersistentRoot(workingCopy),
        captureSelectedCharacter: () => workingCopy.characters[0] ?? null,
        captureCharacter: id => workingCopy.characters.find(value => value.chaId === id) ?? null,
        getSelectedCharacterId: () => workingCopy.characters[0]?.chaId,
        getSelectedConversationId: () => workingCopy.characters[0]?.chats[0]?.id,
        replaceDatabase: next => { workingCopy = next },
        publishCharacter: next => { workingCopy.characters[0] = next },
        publishConversation: (_characterId, value, nextCharacter) => {
            if (nextCharacter) workingCopy.characters[0] = nextCharacter
            else workingCopy.characters[0].chats[0] = value
        },
        canUseWindowedSelectedConversation: () => true,
        isConversationOperationActive: () => false,
    }
    h.runtime = createPersistentDataRuntime({ store, state, prepareDatabase: async candidate => candidate, onLocalRevision: revision => notifyLocalPersistentRevision(revision) })
    await h.runtime.initializeActiveWorkingSet(workingCopy)
    expect(h.runtime.getSelectedConversationMode()).toBe('windowed')
}

async function edit(name: string) {
    workingCopy.username = name
    h.runtime.markPersistentDataDirty(1)
    await h.runtime.flushPendingData('server-sync-runtime-edit')
}

beforeEach(async () => {
    vi.resetModules(); h.invoke.mockReset(); h.events.clear(); h.applyError = undefined
    Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true })
    h.invoke.mockImplementation(async (command: string) => {
        if (command === 'pds_lww_binding_state') return structuredClone(binding)
        if (command === 'server_sync_status') return { configured: true, writerId: 'writer', bindingAuthority: binding.targetAuthority, libraryId: 'library', deviceId: 'device' }
        if (command === 'server_sync_lww_pull') return { bindingAuthority: binding.targetAuthority, requestId: `pull-${pulls()}`, changes: [] }
        if (command === 'pds_lww_apply_receive') {
            // Native errors cross the IPC boundary as plain objects.
            if (h.applyError) throw h.applyError
            return { revision: h.runtime.revision, affectedKeys: [], heldKeys: [], deferredKeys: [] }
        }
        return null
    })
    await createRuntime()
    production = await import('./serverSyncProduction')
    production.initializeNativeSyncBindings()
})
afterEach(() => { production?.disposeNativeSyncBindings() })

it('pulls when the bound library opens, and not again for local saves that renew the open conversation', async () => {
    await production.installServerSyncProduction(); await settle()
    const opened = pulls()
    expect(opened).toBeGreaterThan(0)
    const revision = h.runtime.revision
    for (const name of ['first edit', 'second edit', 'third edit']) { await edit(name); await settle() }
    expect(h.runtime.revision).toBe(revision + 3)
    expect(pulls()).toBe(opened)
    expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: false, error: '' })
})

it('stops on a local validation failure from the native store and ignores remote hints until retry', async () => {
    h.applyError = { code: 'validation', message: 'lww page changes a unit outside the library' }
    await production.installServerSyncProduction(); await settle()
    const stopped = pulls()
    expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: true, error: 'local-validation' })
    h.events.get('risu-server-sync-remote-hint')!()
    await edit('edit after the failure'); await settle()
    expect(pulls()).toBe(stopped)
})

it('tries again after a native storage failure instead of stopping', async () => {
    h.applyError = { code: 'store-error', message: 'database is locked' }
    await production.installServerSyncProduction(); await settle()
    expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: false, error: 'local-storage' })
})
