import { describe, expect, it, vi } from 'vitest'
vi.mock('../platform', () => ({ isTauri: true }))
import type { Database } from './database.svelte'
import { capturePersistentRoot } from './persistentDataRuntime'
import { createPersistenceCanonicalCapture } from './reactivePersistenceCapture.svelte'
import { patchWorkingSetRoot } from './workingSetCatalog'
import { SaveCoordinator, makeStore } from './saveCoordinator.testSupport'

const credential = { id: 'synthetic-account', token: 'synthetic-secret' }
function database(): Database {
    return { characters: [], botPresets: [], account: credential, username: 'Before',
        modules: [{ id: 'module', name: 'Synthetic module' }] } as unknown as Database
}

describe('native account root boundary', () => {
    it('omits account from ordinary and canonical captures', () => {
        const value = database()
        expect(capturePersistentRoot(value)).not.toHaveProperty('account')
        const capture = createPersistenceCanonicalCapture({ root: () => value,
            pluginStorage: () => null, presets: () => [], character: () => null })
        expect(capture.root()).not.toContain('synthetic-secret')
    })

    it('retains the device account and equal nested root identities on foreign root publication', () => {
        const value = database()
        const modules = value.modules
        patchWorkingSetRoot(value, { username: 'After', modules: structuredClone(modules), account: { id: 'foreign' } } as any)
        expect(value.account).toBe(credential)
        expect(value.modules).toBe(modules)
        expect(value.username).toBe('After')
        patchWorkingSetRoot(value, { username: 'After' } as any)
        expect(value.account).toBe(credential)
        expect(value).not.toHaveProperty('modules')
    })

    it('initializing from a database containing the overlay does not create account-removal writes', async () => {
        const value = database()
        const store = makeStore(vi.fn(async () => ({ revision: 2 })))
        const coordinator = new SaveCoordinator({ store, captureRoot: () => capturePersistentRoot(value),
            captureSelectedCharacter: () => null, replaceDatabase: () => undefined })
        coordinator.initialize(1, value)
        await coordinator.flushPendingDataLocally('unchanged native root')
        expect(store.commit).not.toHaveBeenCalled()
        value.account = { ...credential, token: 'new-synthetic-secret' } as any
        coordinator.markPersistentDataDirty(0)
        await coordinator.flushPendingDataLocally('account only')
        expect(store.commit).not.toHaveBeenCalled()
    })
})
