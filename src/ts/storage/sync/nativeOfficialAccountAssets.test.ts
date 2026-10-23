import { describe, expect, it, vi } from 'vitest'
import { createNativeOfficialAccountAssets } from './nativeOfficialAccountAssets'
import type { BlobStore } from '../blobStore'
import type { PersistentRoot } from '../persistentDataStore'
import { createLeaseStub, createStoreStub } from '../persistentDataStore.testSupport'
import { nativeOfficialAccountKeys } from './nativeOfficialAccountFlow'
import type { NativeDeviceSettings } from '../nativeDeviceSettings'
import type { AccountStorage } from '../accountStorage'
import type { NativeFileJobDependencies, NativeFileJobStatus, NativeOfficialAssetRestoreProgress, NativeOfficialAssetSettled } from '../nativeFileJobs'
import { BackgroundExecutionExpiredError } from '../../iosNative'
import { AssetAliasSizeMismatchError } from '../assetRepository'
import { createAccountScopedOfficialAssetLedger, createOfficialAssetLedger, type LedgerStorage } from './officialAssetLedger'

vi.mock('../database.svelte', () => ({
    getDatabase: () => { throw new Error('Asset recovery must use the pinned store') },
    presetTemplate: {},
}))
vi.mock('../../globalApi.svelte', () => ({ forageStorage: {} }))

type Stop = NonNullable<NativeOfficialAssetRestoreProgress['reason']>
type Reply = NativeOfficialAssetSettled | 'hold' | { stop: Stop; failed?: boolean }
interface AssetJob {
    request: Record<string, unknown>
    keys: string[]
    pages: number[]
    taken: number
    held: number
    settled: { index: number; result: NativeOfficialAssetSettled }[]
    state: NativeFileJobStatus['state']
    reason?: Stop
    cancelled: boolean
}

/** The native asset restore job at the Tauri boundary: each status poll answers the next enqueued keys. */
function nativeAssetJobs(install: (key: string) => void) {
    const runs: AssetJob[] = []
    const jobs = new Map<string, AssetJob>()
    let reply: (key: string, job: AssetJob) => Reply = key => key === 'assets/missing' ? 'missing' : 'installed'
    let perPoll = 1
    const step = (job: AssetJob) => {
        for (let answered = 0; answered < perPoll && job.state === 'running'; answered++) {
            if (job.cancelled) { job.state = 'cancelled'; return }
            if (job.taken === job.keys.length) {
                if (job.keys.length === job.request.keyCount && job.held === 0) job.state = 'succeeded'
                return
            }
            const index = job.taken++
            const answer = reply(job.keys[index], job)
            if (answer === 'hold') { job.held++; continue }
            if (typeof answer === 'object') {
                if (answer.failed) job.settled.push({ index, result: 'failed' })
                job.reason = answer.stop
                job.state = 'succeeded'
                return
            }
            if (answer === 'installed') install(job.keys[index])
            job.settled.push({ index, result: answer })
        }
    }
    const status = (jobId: string, job: AssetJob): NativeFileJobStatus => {
        const count = (result: NativeOfficialAssetSettled) => job.settled.filter(entry => entry.result === result).length
        return {
            jobId, kind: 'restore-official-account-assets', state: job.state, phase: job.state === 'running' ? 'reading-source' : 'complete',
            progress: { completedBytes: 0, completedItems: job.settled.length, totalItems: job.keys.length },
            officialAssetRestore: { installed: count('installed'), missing: count('missing'), failed: count('failed'),
                bytes: 3 * count('installed'), active: job.held, limit: 1, ...job.reason ? { reason: job.reason } : {} },
        }
    }
    const invoke = vi.fn(async (command: string, args: Record<string, unknown> = {}) => {
        if (command === 'native_file_job_start') {
            const job: AssetJob = { request: args.request as Record<string, unknown>, keys: [], pages: [], taken: 0, held: 0, settled: [], state: 'running', cancelled: false }
            const jobId = `asset-job-${runs.push(job)}`
            jobs.set(jobId, job)
            return { jobId }
        }
        const jobId = args.jobId as string
        const job = jobs.get(jobId)
        if (!job) throw { code: 'invalid-input', message: 'Synthetic job is missing' }
        switch (command) {
            case 'native_official_asset_restore_enqueue': {
                const keys = args.keys as string[]
                if (keys.length > 512 || job.keys.length + keys.length > (job.request.keyCount as number)) throw { code: 'invalid-input', message: 'Synthetic page is too large' }
                if (job.state !== 'running') return false
                job.pages.push(keys.length)
                job.keys.push(...keys)
                return true
            }
            case 'native_official_asset_restore_settled': {
                const offset = args.offset as number, limit = args.limit as number
                if (limit < 1 || limit > 512) throw { code: 'invalid-input', message: 'Synthetic page is too large' }
                return { entries: job.settled.slice(offset, offset + limit), total: job.settled.length }
            }
            case 'native_file_job_status':
                step(job)
                return status(jobId, job)
            case 'native_file_job_cancel':
                job.cancelled = true
                return 'requested'
            case 'native_file_job_forget':
                if (job.state === 'running') throw { code: 'job-active', message: 'Synthetic job is active' }
                jobs.delete(jobId)
                return true
        }
        throw new Error(`Unexpected command: ${command}`)
    })
    const dependencies = { isTauri: () => true, invoke, wait: () => new Promise<void>(resolve => setTimeout(resolve, 0)) } satisfies NativeFileJobDependencies
    return { runs, jobs, dependencies, reply: (next: typeof reply) => { reply = next }, answerPerPoll: (count: number) => { perPoll = count } }
}

const rejected = (job: AssetJob): Reply => job.request.reauthenticated ? { stop: 'authentication', failed: true } : { stop: 'reauthentication-needed' }

let harnessSequence = 0
function harness(extra = 0, concurrency: 1 | 2 = 1) {
    let library = 'library-1'
    let accountId = 'account'
    let token = 'token-1'
    let renews = true
    const values = new Map<string, Uint8Array>()
    const storedSettings = new Map<string, unknown>()
    const settings: NativeDeviceSettings = {
        get: async key => storedSettings.get(key) ?? null,
        readMany: async keys => keys.map(key => storedSettings.get(key) ?? null),
        set: async (key, value) => { storedSettings.set(key, value) },
        patch: async () => { throw new Error('unexpected patch') },
    }
    const lease = createLeaseStub({
        revision: 7,
        readRoot: async () => ({ revision: 7, value: {
            userIcon: 'assets/avatar', customBackground: 'assets/background',
            modules: [{ assets: [['emotion', 'assets/emotion'], ['missing', 'assets/missing'], ...Array.from({ length: extra }, (_, i) => ['extra', `assets/extra-${i}`])] }],
        } as unknown as PersistentRoot }),
        queryPluginStorage: async () => ({ revision: 7, items: [] }),
        queryCharacters: async () => ({ revision: 7, items: [] }),
        release: vi.fn(async () => {}),
    })
    const store = createStoreStub({ readLibraryIdentity: async () => library, readRoot: async () => ({ revision: 7, value: {} as PersistentRoot }), acquireRevision: async () => lease })
    const blobs = {
        statMany: vi.fn(async (keys: string[]) => keys.map(key => values.has(key) ? {} : null)),
    } as unknown as BlobStore
    const jobs = nativeAssetJobs(key => { values.set(key, Uint8Array.of(1, 2, 3)) })
    const account: Pick<AccountStorage, 'readAssetsFromNative'> = {
        readAssetsFromNative: async attempt => {
            if (await attempt({ credential: { kind: 'risu-auth', token }, reauthenticated: false }) === 'done') return 'done'
            if (!renews) return 'unauthenticated'
            token = 'token-2'
            return await attempt({ credential: { kind: 'risu-auth', token }, reauthenticated: true }) === 'done' ? 'done' : 'unauthenticated'
        },
    }
    const ledgerValues = new Map<string, string>()
    const ledgerStorage: LedgerStorage = { getItem: key => ledgerValues.get(key) ?? null, setItem: (key, value) => { ledgerValues.set(key, value) }, removeItem: key => { ledgerValues.delete(key) } }
    const accountLedger = createAccountScopedOfficialAssetLedger(ledgerStorage, () => accountId)
    const ledger = { record: vi.fn(accountLedger.record), publishedAs: vi.fn(), reset: vi.fn(), clear: vi.fn() }
    const baseUrl = `https://synthetic-${harnessSequence++}.invalid`
    const dependencies = { settings, store, getAccountId: () => accountId, account, ledger, resolveBlobs: async () => blobs, flushMetadata: vi.fn(async () => {}), baseUrl, concurrency, jobs: jobs.dependencies }
    return { values, settings, lease, blobs, jobs, ledger, dependencies, baseUrl, assets: createNativeOfficialAccountAssets(dependencies),
        publishedAs: (account: string, key: string) => createOfficialAssetLedger(ledgerStorage, account).publishedAs(key),
        replaceLibrary: () => { library = 'library-2' }, changeAccount: () => { accountId = 'other' }, refuseLogin: () => { renews = false } }
}

describe('native restored account assets', () => {
    it('preserves legacy backslash keys through recovery and installation', async () => {
        const h = harness()
        h.lease.readRoot = async () => ({ revision: 7, value: { userIcon: 'assets\\legacy.gif' } as PersistentRoot })
        await h.assets.prepare('account'); await h.assets.bind('account')
        expect(await h.assets.inspect('account')).toMatchObject({ kind: 'interrupted' })
        expect(await h.assets.complete('account')).toMatchObject({ state: 'succeeded', counts: { installed: 1 } })
        expect(h.jobs.runs[0].keys).toEqual(['assets\\legacy.gif'])
    })

    it('keeps background expiry resumable instead of persisting a user stop', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        const controller = new AbortController()
        controller.abort(new BackgroundExecutionExpiredError())
        expect(await h.assets.complete('account', { signal: controller.signal })).toMatchObject({ state: 'cancelled' })
        expect(await h.assets.inspect('account')).toMatchObject({ kind: 'interrupted' })
    })

    it('reports receipt mismatch separately from a local presence-check failure', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        const onProgress = vi.fn()
        await expect(h.assets.complete('account', { operationId: 'obsolete', onProgress })).rejects.toMatchObject({ reason: 'receipt' })
        expect(onProgress.mock.calls.at(-1)?.[0]).toMatchObject({ state: 'failed', reason: 'receipt' })
        vi.mocked(h.blobs.statMany!).mockRejectedValueOnce(new Error('Synthetic presence-check failure'))
        expect(await h.assets.complete('account')).toMatchObject({ state: 'failed', reason: 'checking' })
    })

    it('redownloads a size-mismatched alias while retaining healthy local assets', async () => {
        const h = harness()
        h.values.set('assets/avatar', Uint8Array.of(9))
        h.values.set('assets/background', Uint8Array.of(8))
        const stat = vi.mocked(h.blobs.statMany!).getMockImplementation()!
        vi.mocked(h.blobs.statMany!).mockImplementation(async keys => {
            if (keys.includes('assets/avatar')) throw new AssetAliasSizeMismatchError('assets/avatar')
            return stat(keys)
        })
        await h.assets.prepare('account'); await h.assets.bind('account')
        expect(await h.assets.complete('account')).toMatchObject({ state: 'succeeded', counts: {
            alreadyPresent: 1, installed: 2, missing: 1,
        } })
        expect(h.jobs.runs[0].keys).toEqual(['assets/avatar', 'assets/emotion', 'assets/missing'])
    })

    it('starts one native job for the remaining keys and records each installed key', async () => {
        const h = harness()
        await h.assets.prepare('account')
        await h.assets.bind('account')
        expect(await h.assets.complete('account')).toMatchObject({ state: 'succeeded', counts: { required: 4, alreadyPresent: 0, installed: 3, missing: 1, failed: 0, notAttempted: 0 } })
        expect(h.jobs.runs.map(run => run.request)).toEqual([{
            kind: 'restore-official-account-assets', baseUrl: h.baseUrl, credential: { kind: 'risu-auth', token: 'token-1' },
            accountId: 'account', expectedLibrary: 'library-1', keyCount: 4, concurrency: 1, reauthenticated: false,
        }])
        expect(h.ledger.record.mock.calls).toEqual(['assets/avatar', 'assets/background', 'assets/emotion'].map(key => [key, key]))
        expect(h.jobs.jobs.size).toBe(0)
        expect(h.lease.release).toHaveBeenCalledOnce()
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toBeNull()
    })

    it('sends keys and reads settled results in pages the native job accepts', async () => {
        const h = harness(600)
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.jobs.answerPerPoll(1000)
        expect(await h.assets.complete('account')).toMatchObject({ state: 'succeeded', counts: { installed: 603, missing: 1, notAttempted: 0 } })
        expect(h.jobs.runs[0].pages).toEqual([512, 92])
        expect(h.ledger.record).toHaveBeenCalledTimes(603)
    })

    it('reauthenticates once and continues with only the keys the first job left unsettled', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.jobs.reply((key, job) => key === 'assets/background' && !job.request.reauthenticated ? rejected(job) : key === 'assets/missing' ? 'missing' : 'installed')
        const outcome = await h.assets.complete('account')
        expect(outcome).toMatchObject({ state: 'succeeded', counts: { installed: 3, missing: 1 }, transfer: { bytes: 9 } })
        expect(h.jobs.runs.map(run => [run.request.credential, run.request.reauthenticated, run.keys])).toEqual([
            [{ kind: 'risu-auth', token: 'token-1' }, false, ['assets/avatar', 'assets/background', 'assets/emotion', 'assets/missing']],
            [{ kind: 'risu-auth', token: 'token-2' }, true, ['assets/background', 'assets/emotion', 'assets/missing']],
        ])
    })

    it('fails with an authentication reason when reauthentication does not renew the credential', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.jobs.reply((key, job) => key === 'assets/background' ? rejected(job) : 'installed')
        h.refuseLogin()
        expect(await h.assets.complete('account')).toMatchObject({ state: 'failed', reason: 'authentication', counts: { installed: 1, notAttempted: 3 } })
        expect(h.jobs.runs).toHaveLength(1)
    })

    it('keeps in-flight interrupted objects out of the persisted failed set', async () => {
        const h = harness(0, 2)
        await h.assets.prepare('account'); await h.assets.bind('account')
        const controller = new AbortController()
        h.jobs.reply(() => 'hold')
        const running = h.assets.complete('account', { signal: controller.signal })
        await vi.waitFor(() => expect(h.jobs.runs[0]?.held).toBe(4))
        controller.abort()
        expect(await running).toMatchObject({ state: 'cancelled', counts: { failed: 0, notAttempted: 4 }, failedKeys: [] })
        expect(h.jobs.runs[0].state).toBe('cancelled')
        expect(h.jobs.jobs.size).toBe(0)
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toMatchObject({ state: 'stopped', failedKeys: [] })
    })

    it.each(['cancel', 'reopen'] as const)('preserves the previous retry when a new restore ends before activation: %s', async ending => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.jobs.reply((key, job) => key === 'assets/avatar' ? rejected(job) : 'installed')
        await h.assets.complete('account')
        const before = await h.settings.get(nativeOfficialAccountKeys.assetContinuation)
        await h.assets.prepare('account')
        if (ending === 'cancel') await h.assets.cancelPreparation()
        const reopened = createNativeOfficialAccountAssets(h.dependencies)
        expect(await reopened.inspect('account')).toMatchObject({ kind: 'retry' })
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toEqual(before)
        h.jobs.reply(key => key === 'assets/missing' ? 'missing' : 'installed')
        await reopened.complete('account')
        expect(h.values.size).toBe(3)
    })

    it.each([false, true])('discards a first restore interrupted before activation without losing a legacy marker (%s)', async legacy => {
        const h = harness()
        if (legacy) await h.settings.set(nativeOfficialAccountKeys.pendingAssets, { accountId: 'account' })
        await h.assets.prepare('account')
        const reopened = createNativeOfficialAccountAssets(h.dependencies)
        expect(await reopened.inspect('account')).toEqual({ kind: legacy ? 'current-library' : 'none' })
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toBeNull()
        expect(await reopened.inspect('account')).toEqual({ kind: legacy ? 'current-library' : 'none' })
    })

    it.each([false, true])('preserves uncertain activation recovery without a previous receipt (older receipt: %s)', async older => {
        const h = harness()
        await h.assets.prepare('account')
        if (older) {
            const receipt = await h.settings.get(nativeOfficialAccountKeys.assetContinuation) as Record<string, unknown>
            delete receipt.preActivationLibrary
            await h.settings.set(nativeOfficialAccountKeys.assetContinuation, receipt)
        } else h.replaceLibrary()
        const reopened = createNativeOfficialAccountAssets(h.dependencies)
        expect(await reopened.inspect('account')).toEqual({ kind: 'current-library' })
        await reopened.bind('account')
        expect(await reopened.complete('account')).toMatchObject({ state: 'succeeded', counts: { installed: 3 } })
    })

    it('does not revive an old retry after the new database has activated', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        await h.assets.prepare('account')
        h.replaceLibrary()
        const reopened = createNativeOfficialAccountAssets(h.dependencies)
        expect(await reopened.inspect('account')).toEqual({ kind: 'current-library' })
        await reopened.bind('account')
        const receipt = await h.settings.get(nativeOfficialAccountKeys.assetContinuation)
        expect(receipt).toMatchObject({ library: 'library-2', state: 'interrupted' })
        expect(receipt).not.toHaveProperty('previous')
    })

    it.each(['account', 'library', 'operation'] as const)('does not replace a cancellation outcome when its %s ownership became obsolete', async changed => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        const pending = await h.assets.inspect('account')
        if (changed === 'account') h.changeAccount()
        if (changed === 'library') h.replaceLibrary()
        if (changed === 'operation') { await h.assets.prepare('account'); await h.assets.bind('account') }
        const before = await h.settings.get(nativeOfficialAccountKeys.assetContinuation)
        await expect(h.assets.stop('account', pending.operationId!)).resolves.toBeUndefined()
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toEqual(before)
    })

    it('does not automatically assign a released account-only marker to the current library', async () => {
        const h = harness()
        await h.settings.set(nativeOfficialAccountKeys.pendingAssets, { accountId: 'account' })
        expect(await h.assets.inspect('account')).toEqual({ kind: 'current-library' })
        await expect(h.assets.complete('account')).rejects.toMatchObject({ reason: 'receipt' })
        expect(h.values.size).toBe(0)
        await h.assets.prepare('account'); await h.assets.cancelPreparation()
        expect(await h.assets.inspect('account')).toEqual({ kind: 'current-library' })
        await h.assets.prepare('account'); await h.assets.bind('account')
        expect(await h.settings.get(nativeOfficialAccountKeys.pendingAssets)).toBeNull()
        expect(await h.assets.complete('account')).toMatchObject({ state: 'succeeded', counts: { installed: 3 } })
    })

    it('persists a deliberate stop before returning and never mistakes it for interruption', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        expect(await h.assets.inspect('account')).toMatchObject({ kind: 'interrupted' })
        const controller = new AbortController(); controller.abort()
        expect(await h.assets.complete('account', { signal: controller.signal })).toMatchObject({ state: 'cancelled', counts: { notAttempted: 4, failed: 0 } })
        const reopened = createNativeOfficialAccountAssets(h.dependencies)
        expect(await reopened.inspect('account')).toMatchObject({ kind: 'retry' })
        expect(h.jobs.runs).toHaveLength(0)
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toMatchObject({ state: 'stopped' })
    })

    it('freezes the manifest and retries other remaining keys before the persisted failed keys', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.jobs.reply((key, job) => key === 'assets/avatar' ? rejected(job) : 'installed')
        const failed = await h.assets.complete('account')
        expect(await h.assets.inspect('account')).toMatchObject({ kind: 'retry' })
        h.lease.readRoot = async () => ({ revision: 9, value: { userIcon: 'assets/unrelated-new-reference' } as PersistentRoot })
        h.jobs.reply(() => 'installed')
        const reopened = createNativeOfficialAccountAssets(h.dependencies)
        expect(await reopened.complete('account', { operationId: failed.operationId })).toMatchObject({ state: 'succeeded', counts: { required: 4 } })
        expect(h.jobs.runs.at(-1)?.keys.at(-1)).toBe(failed.failedKeys[0])
        expect(h.values.has('assets/unrelated-new-reference')).toBe(false)
        expect(await reopened.inspect('account')).toEqual({ kind: 'none' })
    })

    it('refuses a replaced library and an account change, including a change while the job runs', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.replaceLibrary()
        await expect(h.assets.complete('account')).rejects.toMatchObject({ reason: 'library-changed' })
        expect(await h.assets.inspect('account')).toEqual({ kind: 'none' })
        expect(h.jobs.runs).toHaveLength(0)
        const race = harness()
        await race.assets.prepare('account'); await race.assets.bind('account')
        race.jobs.reply(() => { race.replaceLibrary(); return { stop: 'library-changed', failed: true } })
        expect(await race.assets.complete('account')).toMatchObject({ state: 'failed', reason: 'library-changed' })
        expect(race.ledger.record).not.toHaveBeenCalled()
        const account = harness()
        await account.assets.prepare('account'); await account.assets.bind('account'); account.changeAccount()
        await expect(account.assets.complete('account')).rejects.toMatchObject({ reason: 'account-changed' })
        const running = harness()
        await running.assets.prepare('account'); await running.assets.bind('account')
        running.jobs.reply(() => 'hold')
        const outcome = running.assets.complete('account')
        await vi.waitFor(() => expect(running.jobs.runs[0]?.held).toBe(4))
        running.changeAccount()
        expect(await outcome).toMatchObject({ state: 'failed', reason: 'account-changed' })
        expect(running.jobs.runs[0].state).toBe('cancelled')
        expect(running.jobs.jobs.size).toBe(0)
        const late = harness()
        await late.assets.prepare('account'); await late.assets.bind('account')
        late.jobs.reply(key => { if (key === 'assets/background') late.changeAccount(); return 'installed' })
        expect(await late.assets.complete('account')).toMatchObject({ state: 'failed', reason: 'account-changed' })
        expect(late.publishedAs('account', 'assets/avatar')).toBe('assets/avatar')
        expect(late.publishedAs('other', 'assets/background')).toBeNull()
    })

    it('reports a failed settlement write instead of claiming a durable stopped outcome', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        const write = h.settings.set
        h.settings.set = async (key, value) => { if ((value as { state?: string })?.state === 'stopped') throw new Error('synthetic disk failure'); await write(key, value) }
        const controller = new AbortController(); controller.abort()
        expect(await h.assets.complete('account', { signal: controller.signal })).toMatchObject({ state: 'failed', reason: 'local' })
    })

    it('resumes after interruption and isolates a different signed-in account', async () => {
        const h = harness()
        await h.assets.prepare('account')
        await h.assets.bind('account')
        h.jobs.reply((key, job) => key === 'assets/avatar' ? rejected(job) : 'installed')
        await expect(h.assets.complete('account')).resolves.toMatchObject({ state: 'failed', reason: 'authentication' })
        const resumed = createNativeOfficialAccountAssets(h.dependencies)
        await expect(resumed.complete('other')).rejects.toMatchObject({ reason: 'account-changed' })
        expect(h.jobs.runs).toHaveLength(2)
        h.jobs.reply(() => 'installed')
        await expect(resumed.complete('account')).resolves.toMatchObject({ state: 'succeeded', counts: { installed: 4 } })
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toBeNull()
        expect(h.values.size).toBe(4)
    })

    it('retains the resume marker when the native job stops on a local failure', async () => {
        const h = harness()
        await h.assets.prepare('account')
        await h.assets.bind('account')
        h.jobs.reply(() => ({ stop: 'local', failed: true }))
        await expect(h.assets.complete('account')).resolves.toMatchObject({ state: 'failed', reason: 'local', counts: { failed: 1, notAttempted: 3 } })
        expect(h.ledger.record).not.toHaveBeenCalled()
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toMatchObject({ accountId: 'account', state: 'failed' })
    })

    it.each(['installation', 'metadata'] as const)('keeps installed bytes when stopped during %s and flushes metadata before returning', async stopAt => {
        const h = harness()
        const controller = new AbortController()
        await h.assets.prepare('account')
        await h.assets.bind('account')
        if (stopAt === 'installation') h.jobs.reply(() => { controller.abort(); return 'installed' })
        else h.dependencies.flushMetadata.mockImplementationOnce(async () => { controller.abort() })
        const outcome = await h.assets.complete('account', { signal: controller.signal })
        expect(outcome).toMatchObject({ state: 'cancelled', counts: { installed: stopAt === 'installation' ? 1 : 3, notAttempted: stopAt === 'installation' ? 3 : 0 } })
        expect(h.ledger.record).toHaveBeenCalledTimes(stopAt === 'installation' ? 1 : 3)
        expect(h.dependencies.flushMetadata).toHaveBeenCalledOnce()
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toMatchObject({ state: 'stopped' })
    })

    it('reports failed files separately from a sustained service outage', async () => {
        const h = harness(10)
        await h.assets.prepare('account')
        await h.assets.bind('account')
        let read = 0
        h.jobs.reply(() => read++ < 6 ? 'failed' : 'installed')
        expect(await h.assets.complete('account')).toMatchObject({ state: 'failed', reason: 'files', counts: { failed: 6, installed: 8, notAttempted: 0 } })
        h.values.clear()
        let attempts = 0
        h.jobs.reply(() => ++attempts < 5 ? 'failed' : { stop: 'service', failed: true })
        expect(await h.assets.complete('account')).toMatchObject({ state: 'failed', reason: 'service', counts: { failed: 5, installed: 0, notAttempted: 9 } })
    })
})
