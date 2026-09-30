import { expect, it } from 'vitest'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { IndexedDbPersistentDataStore } from '../indexedDbPersistentDataStore'
import { fixtureDatabase } from './persistentDataFixtures'
import { runContractScenarios } from '../../../../tests/native/contractScenarios'

it('runs native boundary scenarios against IndexedDB with the same assertions', async () => {
    const indexedDB = new IDBFactory()
    const create = async () => {
        const store = new IndexedDbPersistentDataStore('native-contract-parity', indexedDB, IDBKeyRange)
        await store.open()
        return store
    }
    await expect(runContractScenarios(await create(), structuredClone(fixtureDatabase), create)).resolves.toBeUndefined()
})
