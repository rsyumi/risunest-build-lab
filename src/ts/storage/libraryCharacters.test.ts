import { expect, it, vi } from 'vitest'
import { createPersistentStoreHarness } from '../../../tests/support/persistentStoreHarness'
import type { character, Database } from './database.svelte'
import type { PersistentDataStore } from './persistentDataStore'
const factory = vi.hoisted(() => ({ store: null as PersistentDataStore | null }))
vi.mock('./persistentDataStoreFactory', () => ({ getPersistentDataStore: () => factory.store! }))
import { hasLibraryCharacters } from './libraryCharacters'

const entry = (id: string, fields: Record<string, unknown> = {}) =>
    ({ type: 'character', chaId: id, name: id, chats: [], ...fields }) as unknown as character

async function library(characters: character[]) {
    const harness = await createPersistentStoreHarness({ username: 'Fixture', botPresets: [], characters } as unknown as Database)
    factory.store = harness.store
    return harness
}

it('finds no character when neither the list nor the trash holds one', async () => {
    await library([])
    expect(await hasLibraryCharacters()).toBe(false)
})

it.each([
    ['a listed character', entry('listed')],
    ['a group', entry('group', { type: 'group' })],
    ['a character only in the trash', entry('trashed', { trashTime: 1 })],
    ['a group only in the trash', entry('trashed-group', { type: 'group', trashTime: 1 })],
])('finds %s', async (_name, value) => {
    await library([value])
    expect(await hasLibraryCharacters()).toBe(true)
})

it('reads at most one character per list, however large the library', async () => {
    const harness = await library(Array.from({ length: 3 }, (_, index) => entry(`trashed-${index}`, { trashTime: 1 })))
    const query = harness.spy('queryCharacters')
    expect(await hasLibraryCharacters()).toBe(true)
    const pages = await Promise.all(query.mock.results.map((result) => result.value))
    expect(Math.max(...pages.map((page) => page.items.length))).toBe(1)
})

it('counts an unreadable library as holding a character', async () => {
    const harness = await library([])
    harness.failNext('queryCharacters', new Error('unreadable'))
    expect(await hasLibraryCharacters()).toBe(true)
})
