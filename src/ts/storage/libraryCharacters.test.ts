import { beforeEach, expect, it, vi } from 'vitest'
const queryCharacters = vi.hoisted(() => vi.fn())
vi.mock('./persistentDataStoreFactory', () => ({ getPersistentDataStore: () => ({ queryCharacters }) }))
import { hasLibraryCharacters } from './libraryCharacters'

const page = (count: number) => ({ revision: 1, items: Array.from({ length: count }, (_, index) => ({ id: `character-${index}` })) })

beforeEach(() => { queryCharacters.mockReset() })

it('finds no character when neither the list nor the trash holds one', async () => {
    queryCharacters.mockResolvedValue(page(0))
    expect(await hasLibraryCharacters()).toBe(false)
    expect(queryCharacters.mock.calls).toEqual([
        [{ order: 'configured', trash: false, limit: 1 }],
        [{ order: 'configured', trash: true, limit: 1 }],
    ])
})

it('finds a listed character without reading the trash', async () => {
    queryCharacters.mockResolvedValue(page(1))
    expect(await hasLibraryCharacters()).toBe(true)
    expect(queryCharacters).toHaveBeenCalledOnce()
})

it('finds a character that is only in the trash', async () => {
    queryCharacters.mockImplementation(async ({ trash }: { trash: boolean }) => page(trash ? 1 : 0))
    expect(await hasLibraryCharacters()).toBe(true)
    expect(queryCharacters).toHaveBeenCalledTimes(2)
})

it('counts an unreadable library as holding a character', async () => {
    queryCharacters.mockRejectedValue(new Error('unreadable'))
    expect(await hasLibraryCharacters()).toBe(true)
})
