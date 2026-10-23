import type { Database } from '../../src/ts/storage/database.svelte'
export function reactiveFixture(database: Database) {
    const value = $state(database)
    return { database: value }
}
