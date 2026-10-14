import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { vi, type MockInstance } from 'vitest'
import type { Database } from '../../src/ts/storage/database.svelte'
import { IndexedDbPersistentDataStore } from '../../src/ts/storage/indexedDbPersistentDataStore'
import type { PersistentRevisionLease } from '../../src/ts/storage/persistentDataStore'

type Store = IndexedDbPersistentDataStore
type AsyncMethod = {
    [K in keyof Store]: Store[K] extends (...args: never[]) => Promise<unknown> ? K : never
}[keyof Store]
type Method<K extends AsyncMethod> = Store[K] & ((...args: never[]) => Promise<unknown>)
type Args<K extends AsyncMethod> = Store[K] extends (...args: infer A) => unknown ? A : never

export interface PersistentStoreHarness {
    /** A real IndexedDB store on fake-indexeddb, private to this harness. */
    store: Store
    /** The revision after seeding, or the blank store's revision. */
    revision: number
    /** Spies on one store method; calls still reach the real store until a test mocks them. */
    spy<K extends AsyncMethod>(method: K): MockInstance<Method<K>>
    /** Runs `onCall` before every call to `method`, which then reaches the real store. */
    observe<K extends AsyncMethod>(method: K, onCall: (...args: Args<K>) => void | Promise<void>): MockInstance<Method<K>>
    /** Rejects the next call to `method` with `error`; later calls reach the real store. */
    failNext<K extends AsyncMethod>(method: K, error: unknown): void
    /** Holds the next call to `method` until `release()`, then lets it reach the real store. */
    gate<K extends AsyncMethod>(method: K): { entered: Promise<void>; release(): void }
    /** Collects every lease acquired from now on, each with a spy on its `release`. */
    leases(): PersistentRevisionLease[]
}

/** Opens a fresh store, replacing its content with `database` when one is given. */
export async function createPersistentStoreHarness(database?: Database): Promise<PersistentStoreHarness> {
    const store = new IndexedDbPersistentDataStore(
        `persistent-store-harness-${crypto.randomUUID()}`, new IDBFactory(), IDBKeyRange,
    )
    await store.open()
    const revision = database
        ? (await store.replaceFromDatabase(database)).revision
        : (await store.readRoot()).revision
    const spy = <K extends AsyncMethod>(method: K) =>
        vi.spyOn(store, method as never) as unknown as MockInstance<Method<K>>
    const real = <K extends AsyncMethod>(method: K, args: unknown[]) =>
        (IndexedDbPersistentDataStore.prototype[method] as (...values: unknown[]) => Promise<unknown>).apply(store, args)
    return {
        store,
        revision,
        spy,
        observe(method, onCall) {
            return spy(method).mockImplementation((async (...args: Parameters<typeof onCall>) => {
                await onCall(...args)
                return real(method, args)
            }) as never)
        },
        failNext(method, error) {
            spy(method).mockImplementationOnce((() => Promise.reject(error)) as never)
        },
        gate(method) {
            let enter!: () => void
            let release!: () => void
            const entered = new Promise<void>((resolve) => { enter = resolve })
            const released = new Promise<void>((resolve) => { release = resolve })
            spy(method).mockImplementationOnce((async (...args: unknown[]) => {
                enter()
                await released
                return real(method, args)
            }) as never)
            return { entered, release }
        },
        leases() {
            const acquired: PersistentRevisionLease[] = []
            spy('acquireRevision').mockImplementation((async (...args: unknown[]) => {
                const lease = await real('acquireRevision', args) as PersistentRevisionLease
                vi.spyOn(lease, 'release')
                acquired.push(lease)
                return lease
            }) as never)
            return acquired
        },
    }
}
