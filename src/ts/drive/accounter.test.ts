import { beforeEach, expect, it, vi } from 'vitest'
import type { Database } from '../storage/database.svelte'
const f = vi.hoisted(() => ({ fetch: vi.fn(), confirm: vi.fn(), select: vi.fn(), characters: vi.fn() }))
vi.mock('../sionyw', () => ({ fetchProtectedResource: f.fetch }) satisfies Partial<typeof import('../sionyw')>)
vi.mock('../alert', async () => (await import('./backupRestore.testSupport')).alertModule({ alertCheckboxConfirm: f.confirm, alertSelect: f.select }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('../storage/database.svelte', async () => (await import('./backupRestore.testSupport')).databaseModule(() => ({ account: { id: 'account' } }) as Database))
vi.mock('../storage/libraryCharacters', async () => (await import('./backupRestore.testSupport')).libraryCharactersModule(f.characters))
vi.mock('../storage/persistentDataRuntime.svelte', async () => (await import('./backupRestore.testSupport')).persistentRuntimeModule())
vi.mock('../globalApi.svelte', () => ({ AppendableBuffer: class {} }))
vi.mock('../characterCards', async () => (await import('./backupRestore.testSupport')).characterCardsModule())
import { loadRisuAccountBackup } from './accounter'

beforeEach(() => {
    f.fetch.mockReset().mockImplementation(async (path: string) => path === '/hub/backup/list'
        ? { status: 200, json: async () => ['1700000000000'] }
        : { status: 500, text: async () => 'stopped by the test' })
    f.select.mockReset().mockResolvedValue('0')
    f.confirm.mockReset().mockResolvedValue({ confirmed: false, checked: false })
    f.characters.mockReset()
})

it('downloads the selected backup without asking when the library holds no character', async () => {
    f.characters.mockResolvedValue(false)
    await loadRisuAccountBackup()
    expect(f.confirm).not.toHaveBeenCalled()
    expect(f.fetch).toHaveBeenCalledWith('/hub/backup/get', expect.anything())
})

it('asks before replacing a library that holds a character, and declining keeps it', async () => {
    f.characters.mockResolvedValue(true)
    await loadRisuAccountBackup()
    expect(f.confirm).toHaveBeenCalledOnce()
    expect(f.fetch).not.toHaveBeenCalledWith('/hub/backup/get', expect.anything())
})
