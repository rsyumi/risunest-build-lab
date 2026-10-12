import { beforeEach, expect, it, vi } from 'vitest'
const f = vi.hoisted(() => ({ fetch: vi.fn(), confirm: vi.fn(), select: vi.fn(), characters: vi.fn() }))
vi.mock('../sionyw', () => ({ fetchProtectedResource: f.fetch }))
vi.mock('../alert', () => ({
    alertCheckboxConfirm: f.confirm, alertConfirm: vi.fn(), alertError: vi.fn(), alertMd: vi.fn(),
    alertNormal: vi.fn(), alertSelect: f.select, alertWait: vi.fn(),
}))
vi.mock('../platform', () => ({ isTauri: false }))
vi.mock('../storage/database.svelte', () => ({ getDatabase: () => ({ account: { id: 'account' } }) }))
vi.mock('../storage/libraryCharacters', () => ({ hasLibraryCharacters: f.characters }))
vi.mock('../storage/upstreamImport', () => ({ prepareExternalDatabaseImport: vi.fn() }))
vi.mock('../storage/persistentDataRuntime.svelte', () => ({ replacePersistentDatabase: vi.fn() }))
vi.mock('../storage/databaseRestore', () => ({ installAccountBackup: vi.fn() }))
vi.mock('../globalApi.svelte', () => ({ AppendableBuffer: class {} }))
vi.mock('../characterCards', () => ({ hubURL: 'https://hub.test' }))
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
