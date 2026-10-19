import { afterEach, describe, expect, it, vi } from 'vitest'
import { createNativeOfficialAccountAssets } from './nativeOfficialAccountAssets'
import type { BlobStore } from '../blobStore'
import type { PersistentRoot } from '../persistentDataStore'
import { createLeaseStub, createStoreStub } from '../persistentDataStore.testSupport'
import { nativeOfficialAccountKeys } from './nativeOfficialAccountFlow'
import type { NativeDeviceSettings } from '../nativeDeviceSettings'
import type { AccountStorage } from '../accountStorage'
import { AccountReadError } from '../accountReadError'

vi.mock('../database.svelte', () => ({
    getDatabase: () => { throw new Error('Asset recovery must use the pinned store') },
    presetTemplate: {},
}))
vi.mock('../../globalApi.svelte', () => ({ forageStorage: {} }))

afterEach(() => vi.useRealTimers())
let harnessSequence = 0
function harness(extra = 0, concurrency: 1 | 2 = 1) {
    let library = 'library-1'
    let accountId = 'account'
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
        stat: vi.fn(async key => values.has(key) ? {} : null),
        statMany: vi.fn(async (keys: string[]) => keys.map(key => values.has(key) ? {} : null)),
        read: vi.fn(async key => values.get(key) ?? null),
        put: vi.fn(async (key, bytes) => { values.set(key, bytes.slice()) }),
        putVerified: vi.fn(async (key, bytes, _metadata, beforeActivate) => { await beforeActivate?.(); values.set(key, bytes.slice()) }),
        resolveUrl: vi.fn(async key => values.has(key) ? `asset://${key}` : null),
    } as unknown as BlobStore
    const readItem = vi.fn<AccountStorage['readItem']>(async key => key === 'assets/missing'
        ? { kind: 'missing' as const } : { kind: 'value' as const, bytes: Uint8Array.of(1, 2, 3) })
    const ledger = { record: vi.fn(), publishedAs: vi.fn(), reset: vi.fn(), clear: vi.fn() }
    const dependencies = { settings, store, getAccountId: () => accountId, account: { readItem } as Pick<AccountStorage, 'readItem'>, ledger, resolveBlobs: async () => blobs, flushMetadata: vi.fn(async () => {}), origin: `https://synthetic-${harnessSequence++}.invalid`, concurrency }
    return { values, settings, lease, blobs, readItem, ledger, dependencies, assets: createNativeOfficialAccountAssets(dependencies),
        replaceLibrary: () => { library = 'library-2' }, changeAccount: () => { accountId = 'other' } }
}

describe('native restored account assets', () => {
    it('keeps in-flight interrupted objects out of the persisted failed set', async () => {
        const h = harness(0, 2)
        await h.assets.prepare('account'); await h.assets.bind('account')
        const controller = new AbortController()
        let reading = 0
        h.readItem.mockImplementation((_key, options) => new Promise((_resolve, reject) => {
            reading++
            options!.signal!.addEventListener('abort', () => reject(options!.signal!.reason), { once: true })
        }))
        const running = h.assets.complete('account', { signal: controller.signal })
        await vi.waitFor(() => expect(reading).toBe(2))
        controller.abort()
        expect(await running).toMatchObject({ state: 'cancelled', counts: { failed: 0, notAttempted: 4 }, failedKeys: [] })
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toMatchObject({ state: 'stopped', failedKeys: [] })
    })

    it.each(['cancel', 'reopen'] as const)('preserves the previous retry when a new restore ends before activation: %s', async ending => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.readItem.mockRejectedValueOnce(new AccountReadError('http', 401))
        await h.assets.complete('account')
        const before = await h.settings.get(nativeOfficialAccountKeys.assetContinuation)
        await h.assets.prepare('account')
        if (ending === 'cancel') await h.assets.cancelPreparation()
        const reopened = createNativeOfficialAccountAssets(h.dependencies)
        expect(await reopened.inspect('account')).toMatchObject({ kind: 'retry' })
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toEqual(before)
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
        expect(h.readItem).not.toHaveBeenCalled()
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toMatchObject({ state: 'stopped' })
    })

    it('freezes the manifest and retries other remaining keys before the persisted failed keys', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.readItem.mockRejectedValueOnce(new AccountReadError('http', 401))
        const failed = await h.assets.complete('account')
        expect(await h.assets.inspect('account')).toMatchObject({ kind: 'retry' })
        h.lease.readRoot = async () => ({ revision: 9, value: { userIcon: 'assets/unrelated-new-reference' } as PersistentRoot })
        h.readItem.mockClear()
        const reopened = createNativeOfficialAccountAssets(h.dependencies)
        expect(await reopened.complete('account', { operationId: failed.operationId })).toMatchObject({ state: 'succeeded', counts: { required: 4 } })
        expect(h.readItem.mock.calls.at(-1)?.[0]).toBe(failed.failedKeys[0])
        expect(h.values.has('assets/unrelated-new-reference')).toBe(false)
        expect(await reopened.inspect('account')).toEqual({ kind: 'none' })
    })

    it('refuses a replaced library and an account change, including the final activation boundary', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        h.replaceLibrary()
        await expect(h.assets.complete('account')).rejects.toMatchObject({ reason: 'library-changed' })
        expect(await h.assets.inspect('account')).toEqual({ kind: 'none' })
        expect(h.readItem).not.toHaveBeenCalled()
        const race = harness()
        await race.assets.prepare('account'); await race.assets.bind('account')
        vi.mocked(race.blobs.putVerified!).mockImplementation(async (_key, _bytes, _metadata, beforeActivate) => {
            race.replaceLibrary(); await beforeActivate?.(); throw new Error('must not activate')
        })
        expect(await race.assets.complete('account')).toMatchObject({ state: 'failed', reason: 'library-changed' })
        expect(race.values.size).toBe(0)
        expect(race.ledger.record).not.toHaveBeenCalled()
        const account = harness()
        await account.assets.prepare('account'); await account.assets.bind('account'); account.changeAccount()
        await expect(account.assets.complete('account')).rejects.toMatchObject({ reason: 'account-changed' })
    })

    it('reports a failed settlement write instead of claiming a durable stopped outcome', async () => {
        const h = harness()
        await h.assets.prepare('account'); await h.assets.bind('account')
        const write = h.settings.set
        h.settings.set = async (key, value) => { if ((value as { state?: string })?.state === 'stopped') throw new Error('synthetic disk failure'); await write(key, value) }
        const controller = new AbortController(); controller.abort()
        expect(await h.assets.complete('account', { signal: controller.signal })).toMatchObject({ state: 'failed', reason: 'local' })
    })
    it('materializes avatar, emotion and background bytes and settles a missing-only result once', async () => {
        const h = harness()
        await h.assets.prepare('account')
        await h.assets.bind('account')
        expect(await h.assets.complete('account')).toMatchObject({ state: 'succeeded', counts: { required: 4, alreadyPresent: 0, installed: 3, missing: 1, failed: 0, notAttempted: 0 } })
        expect(h.values.size).toBe(3)
        for (const key of ['assets/avatar', 'assets/emotion', 'assets/background']) {
            expect(await h.blobs.read(key)).toEqual(Uint8Array.of(1, 2, 3))
            expect(await h.blobs.resolveUrl(key)).toBe(`asset://${key}`)
            expect(h.ledger.record).toHaveBeenCalledWith(key, key)
        }
        expect(h.lease.release).toHaveBeenCalledOnce()
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toBeNull()
    })

    it('resumes after interruption and isolates a different signed-in account', async () => {
        const h = harness()
        await h.assets.prepare('account')
        await h.assets.bind('account')
        h.readItem.mockRejectedValueOnce(new AccountReadError('http', 401))
        await expect(h.assets.complete('account')).resolves.toMatchObject({ state: 'failed', reason: 'authentication' })
        const resumed = createNativeOfficialAccountAssets(h.dependencies)
        await expect(resumed.complete('other')).rejects.toMatchObject({ reason: 'account-changed' })
        expect(h.readItem).toHaveBeenCalledOnce()
        h.readItem.mockImplementation(async () => ({ kind: 'value', bytes: Uint8Array.of(4) }))
        await expect(resumed.complete('account')).resolves.toMatchObject({ state: 'succeeded', counts: { installed: 4 } })
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toBeNull()
        expect(h.values.size).toBe(4)
    })

    it('retains the resume marker when stored bytes fail verification', async () => {
        const h = harness()
        await h.assets.prepare('account')
        await h.assets.bind('account')
        vi.mocked(h.blobs.putVerified!).mockRejectedValue(new Error('Restored account asset verification failed'))
        await expect(h.assets.complete('account')).resolves.toMatchObject({ state: 'failed', reason: 'local', counts: { failed: 1, notAttempted: 3 } })
        expect(h.ledger.record).not.toHaveBeenCalled()
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toMatchObject({ accountId: 'account', state: 'failed' })
    })

    it.each(['installation', 'metadata'] as const)('keeps installed bytes when stopped during %s and flushes metadata before returning', async stopAt => {
        const h = harness()
        const controller = new AbortController()
        await h.assets.prepare('account')
        await h.assets.bind('account')
        const put = vi.mocked(h.blobs.putVerified!).getMockImplementation()!
        if (stopAt === 'installation') vi.mocked(h.blobs.putVerified!).mockImplementationOnce(async (...args) => {
            const result = await put(...args)
            controller.abort()
            return result
        })
        else h.dependencies.flushMetadata.mockImplementationOnce(async () => { controller.abort() })
        const outcome = await h.assets.complete('account', { signal: controller.signal })
        expect(outcome).toMatchObject({ state: 'cancelled', counts: { installed: stopAt === 'installation' ? 1 : 3, notAttempted: stopAt === 'installation' ? 3 : 0 } })
        expect(h.values.size).toBe(stopAt === 'installation' ? 1 : 3)
        expect(h.dependencies.flushMetadata).toHaveBeenCalledOnce()
        expect(await h.settings.get(nativeOfficialAccountKeys.assetContinuation)).toMatchObject({ state: 'stopped' })
    })

    it('continues past adjacent permanent object errors and stops a sustained service outage', async () => {
        vi.useFakeTimers()
        const h = harness(10)
        await h.assets.prepare('account')
        await h.assets.bind('account')
        let read = 0
        h.readItem.mockImplementation(async () => {
            if (read++ < 6) throw new AccountReadError('http', 404)
            return { kind: 'value', bytes: Uint8Array.of(7) }
        })
        expect(await h.assets.complete('account')).toMatchObject({ state: 'failed', reason: 'files', counts: { failed: 6, installed: 8, notAttempted: 0 } })
        h.values.clear()
        h.readItem.mockRejectedValue(new AccountReadError('http', 429, '0'))
        const running = h.assets.complete('account')
        await vi.advanceTimersByTimeAsync(1000)
        const outage = await running
        expect(outage).toMatchObject({ state: 'failed', reason: 'service', counts: { failed: 5, installed: 0, notAttempted: 9 } })
        const retry = h.assets.complete('account', { previousFailedKeys: outage.failedKeys })
        await vi.advanceTimersByTimeAsync(1000)
        expect(await retry).toMatchObject({ reason: 'service', counts: { failed: 5 } })
    })
})
