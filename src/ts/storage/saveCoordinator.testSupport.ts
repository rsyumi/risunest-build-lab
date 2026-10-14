import { vi } from 'vitest'
import {
    SaveCoordinator as ProductionSaveCoordinator,
    type SaveCoordinatorDependencies,
} from './saveCoordinator'
import type { Database } from './database.svelte'
import type { PersistentDataStore, PersistentRoot, WorkingSetCommit } from './persistentDataStore'
import { createStoreStub } from './persistentDataStore.testSupport'

export function makeDatabase(): Database {
    return {
        username: 'Fixture',
        botPresets: [],
        characters: [
            {
                type: 'character',
                chaId: 'char-a',
                name: 'Alpha',
                chats: [],
            },
        ],
    } as unknown as Database
}

export function deferred<T>() {
    let resolve!: (value: T) => void
    let reject!: (error: unknown) => void
    const promise = new Promise<T>((resolvePromise, rejectPromise) => {
        resolve = resolvePromise
        reject = rejectPromise
    })
    return { promise, resolve, reject }
}

export function makeStore(commit = vi.fn(), members: Partial<PersistentDataStore> = {}) {
    return createStoreStub({ commit, replaceFromDatabase: vi.fn(), ...members })
}

/** Marks a root key that a commit removes. */
export const deletedRootKey = Symbol('deleted root key')

export type CommittedChanges = Omit<WorkingSetCommit, 'root' | 'rootMutations'> & {
    /** Every root key the commit writes, mapped to its new value or `deletedRootKey`. */
    rootChanges: Record<string, unknown>
}

/**
 * One store commit with its root changes in a single shape, whether the commit replaces the
 * whole root or lists root mutations.
 */
export function committedChanges(commit: WorkingSetCommit): CommittedChanges {
    const { root, rootMutations, ...rest } = commit
    const rootChanges: Record<string, unknown> = { ...root }
    for (const mutation of rootMutations ?? [])
        rootChanges[mutation.key] = mutation.type === 'set' ? mutation.value : deletedRootKey
    return { ...rest, rootChanges }
}

export function captureRoot(database: Database): PersistentRoot {
    const {
        characters: _characters,
        botPresets: _botPresets,
        pluginCustomStorage: _pluginCustomStorage,
        pluginStorageMeta: _pluginStorageMeta,
        ...root
    } = database
    return root
}

type TestCoordinatorDependencies = Omit<SaveCoordinatorDependencies, 'captureCharacter'> &
    Partial<Pick<SaveCoordinatorDependencies, 'captureCharacter'>>

export class SaveCoordinator extends ProductionSaveCoordinator {
    constructor(dependencies: TestCoordinatorDependencies) {
        super({
            ...dependencies,
            captureCharacter: dependencies.captureCharacter ?? ((id) => {
                const selected = dependencies.captureSelectedCharacter()
                return selected?.chaId === id ? selected : null
            }),
        })
    }
}
