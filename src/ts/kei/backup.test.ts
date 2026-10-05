import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Database } from '../storage/database.svelte'

let native = false
const vaultRead = vi.fn()
vi.mock('../platform', () => ({ get isTauri() { return native } }))
vi.mock('../storage/nativeAccountCredential', () => ({
    createNativeAccountCredentialVault: () => ({ read: vaultRead }),
}))

let database: Partial<Database>
let persistentDatabase: Partial<Database>
const materializePersistentDatabaseSnapshot = vi.fn()
const getPersistentDataRuntime = vi.fn()
const runNativeKeiBackupJob = vi.fn()
const nativeInvoke = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({ invoke: nativeInvoke }))
vi.mock('../mobileBackgroundTask', () => ({
    runWithMobileBackgroundTask: async (_kind: string, operation: (background: unknown) => Promise<unknown>, signal?: AbortSignal) => operation({ signal, progress: vi.fn() }),
    measuredTaskPercent: () => undefined,
}))

vi.mock('../storage/database.svelte', () => ({
    getDatabase: () => database,
}))
vi.mock('./kei', () => ({
    keiServerURL: () => 'https://kei.example',
}))
vi.mock('../storage/persistentDataRuntime.svelte', () => ({
    materializePersistentDatabaseSnapshot,
    getPersistentDataRuntime,
}))
vi.mock('./nativeBackup', () => ({
    runNativeKeiBackupJob,
}))

const fetchMock = vi.fn(async () => new Response(''))
vi.stubGlobal('fetch', fetchMock)

async function loadSaveDbKei() {
    vi.resetModules()
    return (await import('./backup')).saveDbKei
}

describe('saveDbKei', () => {
    beforeEach(() => {
        native = false
        vaultRead.mockReset()
        vi.useFakeTimers()
        vi.setSystemTime(1_000_000)
        fetchMock.mockClear()
        fetchMock.mockResolvedValue(new Response(''))
        database = {
            account: { id: 'acc', token: 'secret-token', data: {}, kei: true },
        } as Partial<Database>
        persistentDatabase = {
            account: { id: 'acc', token: 'secret-token', data: {}, kei: true },
            characters: [{
                type: 'character',
                chaId: 'complete-character',
                chats: [{ id: 'complete-chat', message: [{ role: 'user', data: 'complete' }] }],
            }],
        } as Partial<Database>
        materializePersistentDatabaseSnapshot.mockReset().mockResolvedValue(persistentDatabase)
        getPersistentDataRuntime.mockReset().mockReturnValue({ revision: 7 })
        runNativeKeiBackupJob.mockReset().mockResolvedValue(false)
    })

    afterEach(() => {
        vi.useRealTimers()
    })

    it('posts the full database as JSON to the kei autobackup route', async () => {
        const saveDbKei = await loadSaveDbKei()

        await saveDbKei()

        expect(materializePersistentDatabaseSnapshot).toHaveBeenCalledWith('kei-auto-backup')
        expect(fetchMock).toHaveBeenCalledTimes(1)
        const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
        expect(url).toBe('https://kei.example/autobackup/save')
        expect(init.method).toBe('POST')
        expect(init.headers).toEqual({ 'Content-Type': 'application/json' })
        expect(JSON.parse(init.body as string)).toEqual({
            token: 'secret-token',
            database: persistentDatabase,
        })
    })

    it.each(['capability-unavailable', 'native-lease-unavailable', 'generic', 'job-capacity', 'cancelled', 'accepted-failure'])(
        'ends the real native %s attempt without renderer fallback and keeps the cadence', async scenario => {
            native = true
            const { nativePersistentRevisionLease } = await import('../storage/nativePersistentExport')
            const release = vi.fn(async () => undefined)
            const lease = { revision: 7, release, ...(scenario === 'native-lease-unavailable' ? {} : {
                [nativePersistentRevisionLease]: 'lease-7',
            }) }
            const runtime = {
                revision: 7,
                flushPendingData: vi.fn(async () => undefined),
                store: { acquireRevision: vi.fn(async () => lease) },
            }
            getPersistentDataRuntime.mockReturnValue(runtime)
            const actual = await vi.importActual<typeof import('./nativeBackup')>('./nativeBackup')
            runNativeKeiBackupJob.mockImplementation(actual.runNativeKeiBackupJob)
            const failure = scenario === 'generic' ? new Error('synthetic native failure') : {
                code: scenario,
                message: 'synthetic native refusal',
            }
            nativeInvoke.mockReset().mockImplementation(async command => {
                if (command === 'native_file_job_start') {
                    if (scenario === 'cancelled' || scenario === 'accepted-failure') return { jobId: 'kei-job' }
                    throw failure
                }
                if (command === 'native_file_job_status') return {
                    state: scenario === 'cancelled' ? 'cancelled' : 'failed',
                    progress: { completedBytes: 0 },
                    error: { code: 'transport-failed', message: 'synthetic upload failure' },
                }
                if (command === 'native_file_job_forget') return true
                throw new Error('unexpected command')
            })
            const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
            try {
                const saveDbKei = await loadSaveDbKei()
                await saveDbKei()
                await saveDbKei()
                expect(runNativeKeiBackupJob).toHaveBeenCalledOnce()
                expect(release).toHaveBeenCalledOnce()
                expect(materializePersistentDatabaseSnapshot).not.toHaveBeenCalled()
                expect(vaultRead).not.toHaveBeenCalled()
                expect(fetchMock).not.toHaveBeenCalled()
                expect(consoleError).toHaveBeenCalledExactlyOnceWith('Kei auto backup failed:',
                    scenario === 'generic' ? failure : expect.objectContaining(
                        scenario === 'cancelled' ? { name: 'AbortError' } : {
                            code: scenario === 'accepted-failure' ? 'transport-failed' : scenario,
                        },
                    ))
                if (scenario === 'cancelled' || scenario === 'accepted-failure') {
                    expect(nativeInvoke).toHaveBeenLastCalledWith('native_file_job_forget', { jobId: 'kei-job' })
                } else {
                    expect(nativeInvoke.mock.calls.some(([command]) => command === 'native_file_job_forget')).toBe(false)
                }
                vi.advanceTimersByTime(5 * 60000)
                await saveDbKei()
                expect(runNativeKeiBackupJob).toHaveBeenCalledTimes(2)
            } finally {
                consoleError.mockRestore()
            }
        },
    )

    it('refuses a false native result without reading the fallback vault or database', async () => {
        native = true
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        try {
            const saveDbKei = await loadSaveDbKei()
            await saveDbKei()
            expect(vaultRead).not.toHaveBeenCalled()
            expect(materializePersistentDatabaseSnapshot).not.toHaveBeenCalled()
            expect(fetchMock).not.toHaveBeenCalled()
            expect(consoleError).toHaveBeenCalledWith('Kei auto backup failed:',
                expect.objectContaining({ message: 'Native KEI backup did not complete' }))
        } finally {
            consoleError.mockRestore()
        }
    })

    it('uses the native job without materializing the database when it is available', async () => {
        const saveDbKei = await loadSaveDbKei()
        runNativeKeiBackupJob.mockResolvedValueOnce(true)

        await saveDbKei()

        expect(runNativeKeiBackupJob).toHaveBeenCalledWith({
            runtime: { revision: 7 },
            url: 'https://kei.example/autobackup/save',
            accountId: 'acc',
            token: 'secret-token',
        })
        expect(materializePersistentDatabaseSnapshot).not.toHaveBeenCalled()
        expect(fetchMock).not.toHaveBeenCalled()
    })

    it('keeps the JavaScript backup on the web', async () => {
        const actual = await vi.importActual<typeof import('./nativeBackup')>('./nativeBackup')
        runNativeKeiBackupJob.mockImplementation(actual.runNativeKeiBackupJob)
        const saveDbKei = await loadSaveDbKei()

        await saveDbKei()

        expect(runNativeKeiBackupJob).toHaveBeenCalledOnce()
        expect(materializePersistentDatabaseSnapshot).toHaveBeenCalledWith('kei-auto-backup')
        expect(fetchMock).toHaveBeenCalledTimes(1)
    })

    it('does not retry in JavaScript after an accepted native job fails', async () => {
        const saveDbKei = await loadSaveDbKei()
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        runNativeKeiBackupJob.mockRejectedValueOnce(new Error('native request failed'))

        await saveDbKei()

        expect(materializePersistentDatabaseSnapshot).not.toHaveBeenCalled()
        expect(fetchMock).not.toHaveBeenCalled()
        expect(consoleError).toHaveBeenCalledWith(
            'Kei auto backup failed:',
            expect.objectContaining({ message: 'native request failed' }),
        )
        consoleError.mockRestore()
    })

    it('does nothing without an account or with the kei flag off', async () => {
        const saveDbKei = await loadSaveDbKei()

        database = {}
        await saveDbKei()
        database = { account: { id: 'acc', token: 'secret-token', data: {} } } as Partial<Database>
        await saveDbKei()

        expect(fetchMock).not.toHaveBeenCalled()
        expect(materializePersistentDatabaseSnapshot).not.toHaveBeenCalled()
    })

    it('sends at most one backup per five minutes', async () => {
        const saveDbKei = await loadSaveDbKei()

        await saveDbKei()
        vi.advanceTimersByTime(5 * 60000 - 1)
        await saveDbKei()
        expect(fetchMock).toHaveBeenCalledTimes(1)

        vi.advanceTimersByTime(1)
        await saveDbKei()
        expect(fetchMock).toHaveBeenCalledTimes(2)
    })

    it('swallows network failures instead of surfacing an unhandled rejection', async () => {
        const saveDbKei = await loadSaveDbKei()
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        fetchMock.mockRejectedValueOnce(new Error('offline'))

        await expect(saveDbKei()).resolves.toBeUndefined()
        await vi.waitFor(() => expect(consoleError).toHaveBeenCalled())
        consoleError.mockRestore()
    })

    it('does not publish when the live account and materialized snapshot disagree', async () => {
        const saveDbKei = await loadSaveDbKei()
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        persistentDatabase.account = {
            id: 'different-account',
            token: 'different-token',
            data: {},
            kei: true,
        }

        await saveDbKei()

        expect(fetchMock).not.toHaveBeenCalled()
        expect(consoleError).toHaveBeenCalledWith(
            'Kei auto backup failed:',
            expect.objectContaining({ message: 'Kei account changed during backup materialization' }),
        )
        consoleError.mockRestore()
    })
})
