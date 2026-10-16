import { beforeEach, expect, it, vi } from 'vitest'
import type { Database } from '../storage/database.svelte'
import { encodeRisuSaveLegacy } from '../storage/risuSave'
import { coldStorageHeader } from '../process/coldstorageData'
import { changeLanguage, language } from 'src/lang'
const f = vi.hoisted(() => ({
    fetch: vi.fn(), confirm: vi.fn(), select: vi.fn(), characters: vi.fn(), readCold: vi.fn(), error: vi.fn(),
    installed: null as Database | null,
}))
vi.mock('../sionyw', () => ({ fetchProtectedResource: f.fetch }) satisfies Partial<typeof import('../sionyw')>)
vi.mock('../alert', async () => (await import('./backupRestore.testSupport')).alertModule({ alertCheckboxConfirm: f.confirm, alertSelect: f.select, alertError: f.error }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('../storage/database.svelte', async () => (await import('./backupRestore.testSupport')).databaseModule(() => ({ account: { id: 'account' } }) as Database))
vi.mock('../storage/libraryCharacters', async () => (await import('./backupRestore.testSupport')).libraryCharactersModule(f.characters))
vi.mock('../storage/persistentDataRuntime.svelte', async () => (await import('./backupRestore.testSupport')).persistentRuntimeModule({
    replacePersistentDatabase: async (database, _reason, options) => {
        f.installed = database
        options?.onPluginsRestarted?.()
        return { kind: 'committed', revision: 2, projection: 'applied' }
    },
}))
vi.mock('../process/coldstorage.svelte', async () => ({
    ...(await import('./backupRestore.testSupport')).coldStorageModule(),
    getAccountColdStorageItem: f.readCold,
}))
vi.mock('../globalApi.svelte', async () => ({ AppendableBuffer: (await import('../appendableBuffer')).AppendableBuffer }))
vi.mock('../characterCards', async () => (await import('./backupRestore.testSupport')).characterCardsModule())
import { loadRisuAccountBackup } from './accounter'

beforeEach(() => {
    changeLanguage('en')
    f.error.mockReset()
    f.fetch.mockReset().mockImplementation(async (path: string) => path === '/hub/backup/list'
        ? { status: 200, json: async () => ['1700000000000'] }
        : { status: 500, text: async () => 'stopped by the test' })
    f.select.mockReset().mockResolvedValue('0')
    f.confirm.mockReset().mockResolvedValue({ confirmed: false, checked: false })
    f.characters.mockReset()
    f.readCold.mockReset()
    f.installed = null
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

function serveColdBackup() {
    const bytes = encodeRisuSaveLegacy({
        characters: [{ type: 'character', chaId: 'cold-character', name: 'Synthetic', coldstorage: 'character-body', chats: [] }],
        botPresets: [],
    })
    f.fetch.mockImplementation(async (path: string) => path === '/hub/backup/list'
        ? { status: 200, json: async () => ['1700000000000'] }
        : new Response(bytes as BodyInit, { status: 200 }))
    f.characters.mockResolvedValue(false)
    f.readCold.mockImplementation(async (key: string) => key === 'character-body' ? {
        character: {
            type: 'character', chaId: 'cold-character', name: 'Synthetic',
            chats: [{ id: 'chat-1', message: [{ role: 'char', data: coldStorageHeader + 'chat-body' }] }],
        },
    } : [{ role: 'char', data: 'Synthetic restored message' }])
}

it('expands account history character and nested chat cold payloads before activation', async () => {
    serveColdBackup()

    await loadRisuAccountBackup()

    expect(f.installed?.characters[0].chats[0].message).toEqual([
        expect.objectContaining({ role: 'char', data: 'Synthetic restored message' }),
    ])
    expect(f.installed?.characters[0]).not.toHaveProperty('coldstorage')
})

it.each([null, { unsupported: true }, { message: 'invalid' }, new Error('Synthetic cold fetch failed')])(
    'keeps the current library when an account history cold payload is unavailable or malformed',
    async (unavailable) => {
        serveColdBackup()
        if (unavailable instanceof Error) f.readCold.mockRejectedValue(unavailable)
        else f.readCold.mockResolvedValue(unavailable)

        await expect(loadRisuAccountBackup()).resolves.toBeUndefined()

        expect(f.installed).toBeNull()
        expect(f.error).toHaveBeenCalledWith(language.accountBackupUnavailable)
    },
)

it('keeps the current library when a restored cold character has an unavailable nested chat', async () => {
    serveColdBackup()
    const readCold = f.readCold.getMockImplementation()!
    f.readCold.mockImplementation(async (key: string) => key === 'chat-body' ? null : readCold(key))

    await expect(loadRisuAccountBackup()).resolves.toBeUndefined()

    expect(f.installed).toBeNull()
    expect(f.error).toHaveBeenCalledWith(language.accountBackupUnavailable)
})

it.each([
    ['ko', '계정 백업의 일부 데이터를 가져오지 못해 복원하지 않았습니다.'],
    ['en', 'The account backup was not restored because some data could not be retrieved.'],
])('shows the unavailable account backup alert in %s without rejecting', async (locale, message) => {
    changeLanguage(locale)
    serveColdBackup()
    f.readCold.mockResolvedValue(null)

    await expect(loadRisuAccountBackup()).resolves.toBeUndefined()

    expect(f.installed).toBeNull()
    expect(f.error).toHaveBeenCalledWith(message)
})
