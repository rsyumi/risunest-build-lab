import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { NativeAccountCredentialVault } from '../nativeAccountCredential'
import {
    createNativeDeviceSettingsBag,
    type NativeDeviceSettings,
} from '../nativeDeviceSettings'
import { createAccountScopedOfficialAssetLedger } from './officialAssetLedger'
import type { OfficialPullResult } from './officialAccountSnapshot'
import {
    createNativeOfficialAccountFlow,
    nativeOfficialAccountKeys,
    type NativeOfficialAccountFlowDependencies,
} from './nativeOfficialAccountFlow'
import { NativeFileJobActivationCommittedError, NativeFileJobActivationUnresolvedError, syntheticNativeFileJobStatus } from '../nativeFileJobs'
import { emptyOfficialAccountAssetCounts } from './officialAccountAssetOutcome'

function createHarness(loggedIn = false, useNativeRestore = false) {
    const events: string[] = []
    const vault = {
        stored: null as unknown,
        read: vi.fn(async () => vault.stored ?? null),
        write: vi.fn(async (credential: unknown) => {
            events.push('credential:write')
            vault.stored = credential
        }),
        clear: vi.fn(async () => {
            events.push('credential:clear')
            vault.stored = null
        }),
    } satisfies NativeAccountCredentialVault & { stored: unknown }
    const publication = {
        publish: vi.fn(async () => { events.push('publish') }),
        dispose: vi.fn(async () => { events.push('dispose') }),
    }
    const adapter = {
        pull: vi.fn(async (): Promise<OfficialPullResult> => ({ kind: 'missing' })),
        pin: vi.fn(async () => publication),
        resetAccountAssociation: vi.fn(),
    }
    const flushPendingData = vi.fn(async () => { events.push('flush') })
    const restart = vi.fn(async () => { events.push('restart') })
    const setRouting = vi.fn((credential) => {
        events.push(credential ? 'route:set' : 'route:clear')
    })
    const clearLegacyFallback = vi.fn(() => events.push('fallback:clear'))
    const flushMetadata = vi.fn(async () => { events.push('metadata:flush') })
    const clearMetadata = vi.fn(async () => { events.push('metadata:clear') })
    const resetAccountSession = vi.fn(() => events.push('session:reset'))
    const nativeRestore = vi.fn<NonNullable<NativeOfficialAccountFlowDependencies['nativeRestore']>>(async (): Promise<
        OfficialPullResult | { kind: 'compatibility-fallback' }
    > => ({
        kind: 'activated',
        revision: 8,
    }))
    const completeRestoreAssets = vi.fn<NonNullable<NativeOfficialAccountFlowDependencies['completeRestoreAssets']>>(async () => ({
        phase: 'finishing', state: 'succeeded', counts: emptyOfficialAccountAssetCounts(), failedKeys: [],
    }))
    let pendingRestoreAccount: string | null = null
    const prepareRestoreAssets = async (accountId: string) => { pendingRestoreAccount = accountId }
    const clearRestoreAssets = vi.fn(async () => { pendingRestoreAccount = null })
    const inspectRestoreAssets = vi.fn<NonNullable<NativeOfficialAccountFlowDependencies['inspectRestoreAssets']>>(async () => ({ kind: 'none' }))
    const flow = createNativeOfficialAccountFlow({
        prepareRestoreAssets, completeRestoreAssets, clearRestoreAssets, inspectRestoreAssets,
        credentialVault: vault,
        adapter,
        initialCredential: loggedIn ? {
            id: 'account-1',
            token: 'legacy-token',
            data: {},
        } : null,
        flushPendingData,
        getRevision: () => 7,
        restart,
        setRouting,
        clearLegacyFallback,
        flushMetadata,
        clearMetadata,
        resetAccountSession,
        ...(useNativeRestore ? { nativeRestore } : {}),
    })
    return {
        get pendingRestoreAccount() { return pendingRestoreAccount },
        completeRestoreAssets, clearRestoreAssets, inspectRestoreAssets,
        adapter,
        clearLegacyFallback,
        clearMetadata,
        events,
        flow,
        flushMetadata,
        flushPendingData,
        publication,
        nativeRestore,
        restart,
        setRouting,
        vault,
    }
}

describe('explicit native official account flow', () => {
    beforeEach(() => vi.restoreAllMocks())

    it('persists only the legacy credential fields without pulling or publishing', async () => {
        const harness = createHarness()

        await expect(harness.flow.login({
            id: 'account-1',
            token: 'legacy-token',
            data: {
                refresh_token: 'refresh',
                access_token: 'access',
                expires_in: 123,
                dpop_private_key: 'must-not-persist',
            },
            useSync: true,
        } as never)).resolves.toEqual({
            id: 'account-1',
            token: 'legacy-token',
            data: {
                refresh_token: 'refresh',
                access_token: 'access',
                expires_in: 123,
            },
        })

        expect(harness.vault.stored).toEqual({
            id: 'account-1',
            token: 'legacy-token',
            data: {
                refresh_token: 'refresh',
                access_token: 'access',
                expires_in: 123,
            },
        })
        expect(harness.adapter.pull).not.toHaveBeenCalled()
        expect(harness.adapter.pin).not.toHaveBeenCalled()
        expect(harness.setRouting).toHaveBeenCalledOnce()
    })

    it('sanitizes and persists credentials returned by native reauthentication', async () => {
        const harness = createHarness(true)

        await expect(harness.flow.reauthenticate(JSON.stringify({
            id: 'account-1',
            token: 'refreshed-token',
            data: {
                refresh_token: 'refresh-2',
                access_token: 'access-2',
                expires_in: 456,
                dpop_private_key: 'must-not-persist',
            },
            useSync: true,
        }))).resolves.toEqual({
            id: 'account-1',
            token: 'refreshed-token',
            data: {
                refresh_token: 'refresh-2',
                access_token: 'access-2',
                expires_in: 456,
            },
        })

        expect(harness.vault.stored).toEqual({
            id: 'account-1',
            token: 'refreshed-token',
            data: {
                refresh_token: 'refresh-2',
                access_token: 'access-2',
                expires_in: 456,
            },
        })
        expect(harness.setRouting).toHaveBeenCalledWith({
            id: 'account-1',
            token: 'refreshed-token',
            data: {
                refresh_token: 'refresh-2',
                access_token: 'access-2',
                expires_in: 456,
            },
        })
    })

    it('resets adapter association when login changes accounts', async () => {
        const harness = createHarness(true)

        await harness.flow.login({
            id: 'account-2',
            token: 'other-token',
            data: {},
        })

        expect(harness.adapter.resetAccountAssociation).toHaveBeenCalledWith('account-2')
    })

    it('rolls back persisted and routed credentials when login routing fails', async () => {
        const harness = createHarness(true)
        const previous = {
            id: 'account-1',
            token: 'legacy-token',
            data: {},
        }
        harness.vault.stored = previous
        harness.setRouting.mockRejectedValueOnce(new Error('route failed'))

        await expect(harness.flow.login({
            id: 'account-2',
            token: 'other-token',
            data: {},
        })).rejects.toMatchObject({ rolledBack: true, cause: new Error('route failed') })

        expect(harness.flow.getToken()).toBe('legacy-token')
        expect(harness.vault.stored).toEqual(previous)
        expect(harness.setRouting).toHaveBeenLastCalledWith(previous)
        expect(harness.adapter.resetAccountAssociation).toHaveBeenCalledWith('account-1')
    })

    it('flushes, pulls once, persists metadata, and cold-restarts after activation', async () => {
        const harness = createHarness(true)
        harness.adapter.pull.mockImplementationOnce(async () => {
            harness.events.push('pull')
            return { kind: 'activated', revision: 8 }
        })

        await expect(harness.flow.restore()).resolves.toEqual({
            kind: 'activated',
            revision: 8,
        })

        expect(harness.adapter.pull).toHaveBeenCalledOnce()
        expect(harness.adapter.pin).not.toHaveBeenCalled()
        expect(harness.events).toEqual([
            'flush',
            'pull',
            'metadata:flush',
            'restart',
        ])
    })

    it('keeps the committed library open when metadata persistence fails', async () => {
        const harness = createHarness(true)
        harness.adapter.pull.mockImplementationOnce(async () => {
            harness.events.push('pull')
            return { kind: 'activated', revision: 8 }
        })
        harness.flushMetadata.mockImplementationOnce(async () => {
            harness.events.push('metadata:flush')
            throw new Error('metadata flush failed')
        })
        harness.restart.mockImplementationOnce(async () => {
            harness.events.push('restart')
            throw new Error('restart failed')
        })

        await expect(harness.flow.restore()).rejects.toMatchObject({
            committedRevision: 8, cause: new Error('metadata flush failed'),
        })

        expect(harness.events).toEqual([
            'flush',
            'pull',
            'metadata:flush',
        ])
        expect(harness.restart).not.toHaveBeenCalled()
    })

    it('requires login and never turns a missing restore into a publish', async () => {
        const loggedOut = createHarness()
        await expect(loggedOut.flow.restore()).rejects.toThrow(
            'Native official account login is required',
        )
        expect(loggedOut.flushPendingData).not.toHaveBeenCalled()
        expect(loggedOut.adapter.pull).not.toHaveBeenCalled()

        const loggedIn = createHarness(true)
        await expect(loggedIn.flow.restore()).resolves.toEqual({ kind: 'missing' })
        expect(loggedIn.adapter.pull).toHaveBeenCalledOnce()
        expect(loggedIn.adapter.pin).not.toHaveBeenCalled()
        expect(loggedIn.restart).not.toHaveBeenCalled()
    })

    it('uses the bounded native snapshot restore when it is configured', async () => {
        const harness = createHarness(true, true)

        await expect(harness.flow.restore()).resolves.toEqual({
            kind: 'activated',
            revision: 8,
        })

        expect(harness.nativeRestore).toHaveBeenCalledWith({
            id: 'account-1',
            token: 'legacy-token',
            data: {},
        }, expect.objectContaining({ onStatus: expect.any(Function) }))
        expect(harness.adapter.pull).not.toHaveBeenCalled()
        expect(harness.events).toEqual(['flush', 'metadata:flush', 'restart'])
    })

    it('uses the existing prepared Web restore only for a native compatibility fallback', async () => {
        const harness = createHarness(true, true)
        harness.nativeRestore.mockResolvedValueOnce({ kind: 'compatibility-fallback' })
        harness.adapter.pull.mockResolvedValueOnce({ kind: 'activated', revision: 9 })

        await expect(harness.flow.restore()).resolves.toEqual({
            kind: 'activated',
            revision: 9,
        })

        expect(harness.nativeRestore).toHaveBeenCalledOnce()
        expect(harness.adapter.pull).toHaveBeenCalledOnce()
        expect(harness.events).toEqual(['flush', 'metadata:flush', 'restart'])
    })

    it('serializes recovery inspection with concurrent restore, publish, and logout operations', async () => {
        const harness = createHarness(true)
        let finishInspection!: () => void
        harness.inspectRestoreAssets.mockImplementationOnce(async () => {
            await new Promise<void>(resolve => { finishInspection = resolve })
            return { kind: 'none' }
        })
        let resolvePull: (result: OfficialPullResult) => void = () => undefined
        harness.adapter.pull.mockImplementationOnce(async () => {
            harness.events.push('pull')
            return new Promise<OfficialPullResult>((resolve) => {
                resolvePull = resolve
            })
        })

        const restore = harness.flow.restore()
        await vi.waitFor(() => expect(harness.adapter.pull).toHaveBeenCalledOnce())
        const inspection = harness.flow.inspectAssets()
        const publish = harness.flow.publish()
        const logout = harness.flow.logout()
        await new Promise((resolve) => setTimeout(resolve, 0))

        expect(harness.inspectRestoreAssets).not.toHaveBeenCalled()
        expect(harness.adapter.pin).not.toHaveBeenCalled()
        expect(harness.vault.clear).not.toHaveBeenCalled()

        resolvePull({ kind: 'missing' })
        await vi.waitFor(() => expect(harness.inspectRestoreAssets).toHaveBeenCalledOnce())
        expect(harness.adapter.pin).not.toHaveBeenCalled()
        expect(harness.vault.clear).not.toHaveBeenCalled()
        finishInspection()
        await expect(Promise.all([restore, inspection, publish, logout])).resolves.toEqual([
            { kind: 'missing' },
            { kind: 'none' },
            undefined,
            undefined,
        ])
        expect(harness.adapter.pin).toHaveBeenCalledOnce()
        expect(harness.vault.clear).toHaveBeenCalledOnce()
        expect(harness.events.indexOf('publish')).toBeLessThan(
            harness.events.indexOf('fallback:clear'),
        )
    })

    it('flushes, pins the current revision, publishes, and always disposes', async () => {
        const harness = createHarness(true)
        const signal = new AbortController().signal

        await harness.flow.publish(signal)

        expect(harness.adapter.pin).toHaveBeenCalledWith(7, { signal, onProgress: undefined, onStatus: undefined, userInitiated: true })
        expect(harness.publication.publish).toHaveBeenCalledWith(signal)
        expect(harness.events).toEqual(['flush', 'publish', 'dispose', 'metadata:flush'])

        harness.events.length = 0
        harness.publication.publish.mockRejectedValueOnce(new Error('offline'))
        harness.publication.dispose.mockImplementationOnce(async () => {
            harness.events.push('dispose')
            throw new Error('dispose failed')
        })
        harness.flushMetadata.mockImplementationOnce(async () => {
            harness.events.push('metadata:flush')
            throw new Error('metadata flush failed')
        })
        await expect(harness.flow.publish()).rejects.toThrow('offline')
        expect(harness.events).toEqual(['flush', 'dispose', 'metadata:flush'])
    })

    it('flushes metadata after pin fails and preserves the pin error', async () => {
        const harness = createHarness(true)
        harness.adapter.pin.mockImplementationOnce(async () => {
            harness.events.push('pin')
            throw new Error('pin failed')
        })
        harness.flushMetadata.mockImplementationOnce(async () => {
            harness.events.push('metadata:flush')
            throw new Error('metadata flush failed')
        })

        await expect(harness.flow.publish()).rejects.toThrow('pin failed')

        expect(harness.events).toEqual(['flush', 'pin', 'metadata:flush'])
        expect(harness.publication.publish).not.toHaveBeenCalled()
        expect(harness.publication.dispose).not.toHaveBeenCalled()
    })

    it('logs out by clearing the vault and the stored metadata without remote work', async () => {
        const harness = createHarness(true)
        harness.vault.stored = { id: 'account-1', token: 'legacy-token', data: {} }

        await harness.flow.logout()

        expect(harness.flushMetadata).toHaveBeenCalledOnce()
        expect(harness.vault.clear).toHaveBeenCalledOnce()
        expect(harness.vault.stored).toBeNull()
        expect(harness.clearMetadata).toHaveBeenCalledOnce()
        expect(harness.events.indexOf('metadata:flush')).toBeLessThan(
            harness.events.indexOf('metadata:clear'),
        )
        expect(harness.clearLegacyFallback).toHaveBeenCalledOnce()
        expect(harness.adapter.resetAccountAssociation).toHaveBeenCalledWith(null)
        expect(harness.setRouting).toHaveBeenCalledWith(null)
        expect(harness.adapter.pull).not.toHaveBeenCalled()
        expect(harness.adapter.pin).not.toHaveBeenCalled()
        await expect(harness.flow.restore()).rejects.toThrow(
            'Native official account login is required',
        )
    })

    it('clears bootstrap metadata mirrors and ledger binding across logout and relogin', async () => {
        const backend = new Map<string, unknown>([
            [nativeOfficialAccountKeys.association, {
                'officialAssociation:account-1': 'persisted-association',
            }],
            [nativeOfficialAccountKeys.assetLedger, {
                'officialPublishedAssets:account-1': JSON.stringify({
                    version: 1,
                    assets: { 'assets/old.png': 'remote/old.png' },
                    cold: {},
                }),
            }],
        ])
        const settings: NativeDeviceSettings = {
            get: vi.fn(async (key) => backend.get(key) ?? null),
            readMany: vi.fn(async (keys: readonly string[]) =>
                keys.map((key) => backend.get(key) ?? null)),
            set: vi.fn(async (key, value) => {
                if (value === null) backend.delete(key)
                else backend.set(key, value)
            }),
            patch: vi.fn(async (key: string, entries: Record<string, string | null>) => {
                const stored = { ...(backend.get(key) as Record<string, string> ?? {}) }
                for (const [entry, value] of Object.entries(entries)) {
                    if (value === null) delete stored[entry]
                    else stored[entry] = value
                }
                backend.set(key, stored)
            }),
        }
        const association = await createNativeDeviceSettingsBag(
            settings,
            nativeOfficialAccountKeys.association,
        )
        const assetLedger = await createNativeDeviceSettingsBag(
            settings,
            nativeOfficialAccountKeys.assetLedger,
        )
        let accountId: string | undefined = 'account-1'
        const ledger = createAccountScopedOfficialAssetLedger(
            assetLedger.storage,
            () => accountId,
        )
        expect(ledger.publishedAs('assets/old.png')).toBe('remote/old.png')
        const flow = createNativeOfficialAccountFlow({
            credentialVault: {
                read: vi.fn(async () => null),
                write: vi.fn(async () => undefined),
                clear: vi.fn(async () => undefined),
            },
            adapter: {
                pull: vi.fn(),
                pin: vi.fn(),
                resetAccountAssociation: vi.fn(),
            },
            initialCredential: { id: 'account-1', token: 'legacy-token', data: {} },
            flushPendingData: vi.fn(),
            getRevision: () => 1,
            restart: vi.fn(),
            setRouting: (credential) => {
                accountId = credential?.id
            },
            clearLegacyFallback: vi.fn(),
            flushMetadata: async () => {
                await association.flush()
                await assetLedger.flush()
            },
            clearMetadata: async () => {
                await association.clear()
                await assetLedger.clear()
                ledger.reset()
            },
            resetAccountSession: vi.fn(),
        })

        await flow.logout()
        await flow.login({ id: 'account-1', token: 'new-token', data: {} })

        expect(association.storage.getItem('officialAssociation:account-1')).toBeNull()
        expect(ledger.publishedAs('assets/old.png')).toBeNull()
        expect(backend.has(nativeOfficialAccountKeys.association)).toBe(false)
        expect(backend.has(nativeOfficialAccountKeys.assetLedger)).toBe(false)
    })
    it('reports rollback failure without claiming the previous vault state survived', async () => {
        const harness = createHarness(true)
        harness.vault.write.mockRejectedValue(new Error('vault offline'))
        await expect(harness.flow.login({ id: 'account-2', token: 'new', data: {} }))
            .rejects.toMatchObject({ name: 'NativeAccountLoginError', rolledBack: false })
    })

    it('propagates cancellation and progress to native restore and compatibility pull', async () => {
        const harness = createHarness(true, true)
        const controller = new AbortController()
        const options = { signal: controller.signal, onStatus: vi.fn(), onBlockingChange: vi.fn() }
        const status = syntheticNativeFileJobStatus({ kind: 'restore-official-account-snapshot' }, 'reading-database')
        harness.nativeRestore.mockImplementationOnce(async (_credential, forwarded) => {
            expect(forwarded.signal).toBe(controller.signal)
            forwarded.onStatus?.(status)
            return { kind: 'compatibility-fallback' }
        })
        await harness.flow.restore(options)
        expect(options.onStatus).toHaveBeenCalledWith(status)
        expect(harness.adapter.pull).toHaveBeenCalledWith(controller.signal)
        controller.abort()
        await expect(harness.flow.restore(options)).rejects.toMatchObject({ name: 'AbortError' })
        expect(harness.nativeRestore).toHaveBeenCalledOnce()
    })

    it('identifies a restart failure as committed activation', async () => {
        const harness = createHarness(true, true)
        harness.restart.mockRejectedValueOnce(new Error('restart failed'))
        await expect(harness.flow.restore()).rejects.toBeInstanceOf(NativeFileJobActivationCommittedError)
    })

    it('preserves database counters and activation identity while asset status remains independently cancellable', async () => {
        const h = createHarness(true, true)
        const native = syntheticNativeFileJobStatus({ kind: 'restore-official-account-snapshot', jobId: 'database-job',
            state: 'succeeded', activationRevision: 8, activationAuthority: 'authority', warningCodes: ['synthetic-warning'],
            result: { revision: 8, sourceBytes: 100, sourceSha256: 'a'.repeat(64), sourceFingerprintKind: 'whole-file-sha256', characterCount: 3, presetCount: 2, warningCodes: ['synthetic-warning'] },
        }, 'reloading-plugins', { counts: { characters: 3, presets: 2 } })
        Object.assign(native, { activationRevision: 8, activationAuthority: 'authority' })
        h.nativeRestore.mockImplementationOnce(async (_credential, options) => {
            options.onStatus?.(native)
            return { kind: 'activated', revision: 8 }
        })
        h.completeRestoreAssets.mockImplementationOnce(async (_account, options) => {
            const outcome = { phase: 'downloading' as const, state: 'cancelled' as const,
                counts: { required: 3, alreadyPresent: 1, installed: 1, missing: 0, failed: 0, notAttempted: 1 }, failedKeys: [],
            }
            options.onProgress?.({ ...outcome, state: 'running' })
            options.onProgress?.(outcome)
            return outcome
        })
        const onStatus = vi.fn()
        await expect(h.flow.restore({ onStatus })).rejects.toMatchObject({ code: 'official-assets-incomplete', committedRevision: 8 })
        const shown = onStatus.mock.calls.at(-1)![0]
        expect(shown).toMatchObject({ jobId: 'database-job', activationAuthority: 'authority', activationRevision: 8, state: 'running',
            detail: { stage: 'downloading-account-assets', counts: { characters: 3, presets: 2 } },
            officialAssets: { state: 'cancelled', counts: { installed: 1, notAttempted: 1 } },
            result: native.result, warningCodes: native.warningCodes,
        })
        expect(h.restart).not.toHaveBeenCalled()
    })

    it('retries only receipt-bound assets and retains the original database result', async () => {
        const h = createHarness(true, true)
        h.adapter.pull.mockImplementation(async () => { throw new Error('Retry must not restore the database') })
        h.nativeRestore.mockImplementation(async () => { throw new Error('Retry must not restore the database') })
        const original = syntheticNativeFileJobStatus({ kind: 'restore-official-account-snapshot', jobId: 'original', activationRevision: 5, activationAuthority: 'binding' }, 'cleaning-up')
        Object.assign(original, { activationRevision: 5, activationAuthority: 'binding' })
        h.completeRestoreAssets.mockImplementationOnce(async (_account, options) => {
            expect(options.operationId).toBe('receipt')
            options.onRestoreStatus?.(original)
            const outcome = { operationId: 'receipt', phase: 'finishing' as const, state: 'failed' as const,
                counts: { ...emptyOfficialAccountAssetCounts(), required: 2, installed: 1, failed: 1 }, failedKeys: ['assets/failed'] }
            options.onProgress?.(outcome)
            return outcome
        })
        const onStatus = vi.fn()
        await expect(h.flow.resumeAssets({ operationId: 'receipt', onStatus })).rejects.toMatchObject({ committedRevision: 5, code: 'official-assets-incomplete' })
        expect(onStatus.mock.calls.at(-1)?.[0]).toMatchObject({ jobId: 'original', activationAuthority: 'binding', officialAssets: { operationId: 'receipt' } })
        expect(h.restart).not.toHaveBeenCalled()
    })

    it('persists metadata without restarting when post-activation asset recovery fails', async () => {
        const harness = createHarness(true, true)
        harness.completeRestoreAssets.mockRejectedValueOnce(new Error('asset offline'))
        const blocking = vi.fn()
        await expect(harness.flow.restore({ onBlockingChange: blocking })).rejects.toMatchObject({
            committedRevision: 8, cause: new Error('asset offline'),
        })
        expect(harness.flushMetadata).toHaveBeenCalledOnce()
        expect(harness.restart).not.toHaveBeenCalled()
        expect(blocking.mock.calls).toEqual([[true], [false]])
        expect(harness.clearRestoreAssets).not.toHaveBeenCalled()
    })

    it.each(['restore', 'resumeAssets'] as const)('keeps a missing-only %s outcome visible without restarting', async operation => {
        const h = createHarness(true, true)
        h.completeRestoreAssets.mockResolvedValue({ phase: 'finishing', state: 'succeeded', failedKeys: [],
            counts: { ...emptyOfficialAccountAssetCounts(), required: 1, missing: 1 } })
        await h.flow[operation]()
        expect(h.flushMetadata).toHaveBeenCalledOnce()
        expect(h.restart).not.toHaveBeenCalled()
    })

    it.each([
        ['pre-activation failure', new Error('offline'), null],
        ['committed activation', new NativeFileJobActivationCommittedError(8, new Error('refresh failed')), 'account-1'],
        ['unresolved activation', new NativeFileJobActivationUnresolvedError(new Error('status unavailable')), 'account-1'],
    ] as const)('retains the correct asset resume intent after %s', async (_label, error, pendingAccount) => {
        const harness = createHarness(true, true)
        harness.nativeRestore.mockRejectedValueOnce(error)
        await expect(harness.flow.restore()).rejects.toBe(error)
        expect(harness.pendingRestoreAccount).toBe(pendingAccount)
        expect(harness.completeRestoreAssets).not.toHaveBeenCalled()
        expect(harness.restart).not.toHaveBeenCalled()
    })

    it('exposes only the current authenticated account identity through login and logout', async () => {
        const harness = createHarness()
        expect(harness.flow.getAccountId()).toBeNull()
        await harness.flow.login({ id: 'vault-account', token: 'synthetic', data: {} })
        expect(harness.flow.getAccountId()).toBe('vault-account')
        await harness.flow.logout()
        expect(harness.flow.getAccountId()).toBeNull()
    })

    it('keeps native logout effective after metadata cleanup fails and retries the cleanup', async () => {
        const harness = createHarness(true)
        const account = { id: 'account-1', token: 'legacy-token', data: {} }
        harness.vault.stored = account
        let routedAccount: typeof account | null = account
        harness.setRouting.mockImplementation(value => { routedAccount = value })
        let metadataPresent = true
        harness.clearMetadata.mockImplementation(async () => { metadataPresent = false })
        harness.clearMetadata.mockRejectedValueOnce(new Error('metadata cleanup failed'))

        await expect(harness.flow.logout()).rejects.toThrow('metadata cleanup failed')

        expect(harness.vault.stored).toBeNull()
        expect(harness.flow.getAccountId()).toBeNull()
        expect(harness.flow.getToken()).toBeNull()
        expect(routedAccount).toBeNull()
        expect(metadataPresent).toBe(true)
        await expect(harness.flow.publish()).rejects.toThrow('login is required')
        await harness.flow.logout()
        expect(metadataPresent).toBe(false)
        expect(routedAccount).toBeNull()
    })

    it('retains native login when logout metadata flush fails before clearing the vault', async () => {
        const harness = createHarness(true)
        const account = { id: 'account-1', token: 'legacy-token', data: {} }
        harness.vault.stored = account
        let routedAccount: typeof account | null = account
        harness.setRouting.mockImplementation(value => { routedAccount = value })
        harness.flushMetadata.mockRejectedValueOnce(new Error('metadata flush failed'))

        await expect(harness.flow.logout()).rejects.toThrow('metadata flush failed')

        expect(harness.vault.stored).toEqual(account)
        expect(harness.flow.getAccountId()).toBe(account.id)
        expect(harness.flow.getToken()).toBe(account.token)
        expect(routedAccount).toEqual(account)
        await harness.flow.logout()
        expect(harness.vault.stored).toBeNull()
        expect(routedAccount).toBeNull()
    })

})
