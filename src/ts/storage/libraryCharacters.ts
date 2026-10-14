import { getPersistentDataStore } from './persistentDataStoreFactory'

/**
 * Whether this library holds a character, counting groups and trashed or archived ones.
 * A library without one has nothing to lose to a replacement; an unreadable one counts as holding one.
 */
export async function hasLibraryCharacters(): Promise<boolean> {
    try {
        const store = getPersistentDataStore()
        for (const trash of [false, true]) {
            if ((await store.queryCharacters({ order: 'configured', trash, limit: 1 })).items.length > 0) return true
        }
        return false
    } catch {
        return true
    }
}
