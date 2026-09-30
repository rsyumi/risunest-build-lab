import type { Database } from '../database.svelte'
import type { NativeAccountCredentialVault } from '../nativeAccountCredential'
import type { DataRevision } from '../persistentDataStore'
import type { PinnedPublication } from '../saveCoordinator'
import { NativeFileJobActivationCommittedError, syntheticNativeFileJobStatus, type NativeFileRestoreJobOptions } from '../nativeFileJobs'
import type { OfficialPublicationOptions } from './officialAccountSnapshot'
import type { OfficialPullResult } from './officialAccountSnapshot'

/**
 * The credential is not here: it belongs to the OS vault, not to a stored
 * value a backup or a restore would carry.
 */
export const nativeOfficialAccountKeys = {
    association: 'official-account.association.v1',
    assetLedger: 'official-account.asset-ledger.v1',
    pendingAssets: 'official-account.pending-assets.v1',
} as const

export type NativeOfficialAccountCredential = NonNullable<Database['account']>

interface NativeOfficialAdapter {
    pull(signal?: AbortSignal): Promise<OfficialPullResult>
    pin(revision: DataRevision, options?: OfficialPublicationOptions): Promise<PinnedPublication>
    resetAccountAssociation(accountId: string | null): void
}

export interface NativeOfficialAccountFlowDependencies {
    credentialVault: NativeAccountCredentialVault
    adapter: NativeOfficialAdapter
    initialCredential: NativeOfficialAccountCredential | null
    flushPendingData(reason: string): Promise<void>
    getRevision(): DataRevision
    restart(): Promise<void>
    setRouting(credential: NativeOfficialAccountCredential | null): void | Promise<void>
    clearLegacyFallback(): void
    flushMetadata(): Promise<void>
    /** Removes the stored account metadata and drops the in-memory copies. */
    clearMetadata(): Promise<void>
    resetAccountSession(): void
    prepareRestoreAssets?(accountId: string): Promise<void>
    completeRestoreAssets?(accountId: string, onProgress?: (completed: number, total: number) => void): Promise<void>
    clearRestoreAssets?(): Promise<void>
    nativeRestore?(credential: NativeOfficialAccountCredential, options: NativeOfficialRestoreOptions): Promise<
        OfficialPullResult | { kind: 'compatibility-fallback' }
    >
}

export type NativeOfficialRestoreOptions = Pick<NativeFileRestoreJobOptions, 'signal' | 'onStatus' | 'onBlockingChange'>

export class NativeAccountLoginError extends Error {
    constructor(readonly rolledBack: boolean, readonly cause: unknown) {
        super('Native official account login failed')
        this.name = 'NativeAccountLoginError'
    }
}

export interface NativeOfficialAccountFlow {
    login(credential: NativeOfficialAccountCredential): Promise<NativeOfficialAccountCredential>
    getAccountId(): string | null
    getToken(): string | null
    reauthenticate(loginResult: string): Promise<NativeOfficialAccountCredential>
    restore(options?: NativeOfficialRestoreOptions): Promise<OfficialPullResult>
    publish(signal?: AbortSignal, onProgress?: OfficialPublicationOptions['onProgress'], onStatus?: OfficialPublicationOptions['onStatus']): Promise<void>
    logout(): Promise<void>
}

export interface NativeOfficialAccountFlowService {
    flow: NativeOfficialAccountFlow
    snapshotRequestReauthentication: {
        reauthenticate(loginResult: string): Promise<NativeOfficialAccountCredential>
    }
}

export function normalizeNativeOfficialAccountCredential(
    value: unknown,
): NativeOfficialAccountCredential | null {
    if (!value || typeof value !== 'object') return null
    const credential = value as Partial<NativeOfficialAccountCredential>
    if (typeof credential.id !== 'string' || typeof credential.token !== 'string') return null
    const data = credential.data && typeof credential.data === 'object' ? credential.data : {}
    return {
        id: credential.id,
        token: credential.token,
        data: {
            ...(typeof data.refresh_token === 'string' ? { refresh_token: data.refresh_token } : {}),
            ...(typeof data.access_token === 'string' ? { access_token: data.access_token } : {}),
            ...(typeof data.expires_in === 'number' ? { expires_in: data.expires_in } : {}),
        },
        ...(typeof credential.kei === 'boolean' ? { kei: credential.kei } : {}),
    }
}

function throwIfAborted(signal?: AbortSignal): void {
    if (signal?.aborted) throw signal.reason ?? new DOMException('The operation was aborted', 'AbortError')
}

export function createNativeOfficialAccountFlowService(
    dependencies: NativeOfficialAccountFlowDependencies,
): NativeOfficialAccountFlowService {
    let credential = dependencies.initialCredential
    let credentialGeneration = 0
    let operationTail = Promise.resolve()
    const serialize = <T>(operation: () => Promise<T>): Promise<T> => {
        const result = operationTail.then(operation, operation)
        operationTail = result.then(() => undefined, () => undefined)
        return result
    }
    const login = async (input: NativeOfficialAccountCredential) => {
        const nextCredential = normalizeNativeOfficialAccountCredential(input)
        if (!nextCredential) throw new Error('Invalid native official account credential')
        const previousCredential = credential
        const accountChanged = previousCredential?.id !== nextCredential.id
        try {
            if (accountChanged) dependencies.resetAccountSession()
            await dependencies.credentialVault.write(nextCredential)
            await dependencies.setRouting(nextCredential)
            if (accountChanged) {
                dependencies.adapter.resetAccountAssociation(nextCredential.id)
            }
        } catch (error) {
            let rolledBack = true
            try {
                if (previousCredential) {
                    await dependencies.credentialVault.write(previousCredential)
                } else {
                    await dependencies.credentialVault.clear()
                }
            } catch { rolledBack = false }
            try {
                await dependencies.setRouting(previousCredential)
            } catch { rolledBack = false }
            if (accountChanged) {
                try {
                    dependencies.adapter.resetAccountAssociation(previousCredential?.id ?? null)
                } catch { rolledBack = false }
            }
            throw new NativeAccountLoginError(rolledBack, error)
        }
        credential = nextCredential
        credentialGeneration += 1
        return nextCredential
    }
    const parseCredential = (loginResult: string): NativeOfficialAccountCredential => {
        let parsed: unknown
        try {
            parsed = JSON.parse(loginResult)
        } catch (error) {
            throw new Error('Invalid native official account credential')
        }
        const parsedCredential = normalizeNativeOfficialAccountCredential(parsed)
        if (!parsedCredential) throw new Error('Invalid native official account credential')
        return parsedCredential
    }
    const reauthenticate = (loginResult: string) => login(parseCredential(loginResult))
    const flow: NativeOfficialAccountFlow = {
        login(input) {
            return serialize(() => login(input))
        },
        getAccountId() {
            return credential?.id ?? null
        },
        getToken() {
            return credential?.token ?? null
        },
        reauthenticate(loginResult) {
            const expectedGeneration = credentialGeneration
            const expectedAccountId = credential?.id
            return serialize(() => {
                if (!expectedAccountId
                    || credentialGeneration !== expectedGeneration
                    || credential?.id !== expectedAccountId) {
                    throw new Error('Native official account session changed during reauthentication')
                }
                return reauthenticate(loginResult)
            })
        },
        restore(options = {}) {
            return serialize(async () => {
                if (!credential) throw new Error('Native official account login is required')
                throwIfAborted(options.signal)
                await dependencies.flushPendingData('native-official-restore')
                throwIfAborted(options.signal)
                await dependencies.prepareRestoreAssets?.(credential.id)
                let result: OfficialPullResult
                try {
                    const nativeResult = dependencies.nativeRestore
                        ? await dependencies.nativeRestore(credential, options)
                        : await dependencies.adapter.pull(options.signal)
                    result = nativeResult.kind === 'compatibility-fallback'
                        ? await dependencies.adapter.pull(options.signal)
                        : nativeResult
                } catch (error) {
                    if (!(error instanceof NativeFileJobActivationCommittedError)) {
                        try { await dependencies.clearRestoreAssets?.() } catch {}
                    }
                    throw error
                }
                if (result.kind === 'activated') {
                    options.onBlockingChange?.(true)
                    let failed = false
                    let originalError: unknown
                    try {
                        await dependencies.completeRestoreAssets?.(credential.id, (completed, total) => {
                            options.onStatus?.(syntheticNativeFileJobStatus(
                                { kind: 'restore-official-account-snapshot' }, 'refreshing-app',
                                { stageCompleted: completed, stageTotal: total, stageUnit: 'items' },
                            ))
                        })
                    } catch (error) {
                        failed = true
                        originalError = error
                    }
                    try {
                        await dependencies.flushMetadata()
                    } catch (error) {
                        if (!failed) { failed = true; originalError = error }
                    }
                    try {
                        await dependencies.restart()
                    } catch (error) {
                        if (!failed) {
                            failed = true
                            originalError = error
                        }
                    }
                    options.onBlockingChange?.(false)
                    if (failed) throw new NativeFileJobActivationCommittedError(result.revision, originalError)
                } else {
                    await dependencies.clearRestoreAssets?.()
                }
                return result
            })
        },
        publish(signal, onProgress, onStatus) {
            return serialize(async () => {
                if (!credential) throw new Error('Native official account login is required')
                throwIfAborted(signal)
                await dependencies.flushPendingData('native-official-publish')
                throwIfAborted(signal)
                let publication: PinnedPublication | null = null
                let failed = false
                let originalError: unknown
                try {
                    publication = await dependencies.adapter.pin(dependencies.getRevision(), { signal, onProgress, onStatus, userInitiated: true })
                    await publication.publish(signal)
                } catch (error) {
                    failed = true
                    originalError = error
                } finally {
                    if (publication) {
                        try {
                            await publication.dispose()
                        } catch (error) {
                            if (!failed) {
                                failed = true
                                originalError = error
                            }
                        }
                    }
                    try {
                        await dependencies.flushMetadata()
                    } catch (error) {
                        if (!failed) {
                            failed = true
                            originalError = error
                        }
                    }
                }
                if (failed) throw originalError
            })
        },
        logout() {
            return serialize(async () => {
                dependencies.clearLegacyFallback()
                dependencies.resetAccountSession()
                await dependencies.flushMetadata()
                await dependencies.credentialVault.clear()
                await dependencies.clearMetadata()
                credential = null
                credentialGeneration += 1
                dependencies.adapter.resetAccountAssociation(null)
                await dependencies.setRouting(null)
            })
        },
    }
    return {
        flow,
        snapshotRequestReauthentication: {
            reauthenticate(loginResult) {
                const currentAccountId = credential?.id
                const nextCredential = parseCredential(loginResult)
                if (!currentAccountId || nextCredential.id !== currentAccountId) {
                    throw new Error('Native official account changed during snapshot reauthentication')
                }
                return login(nextCredential)
            },
        },
    }
}

export function createNativeOfficialAccountFlow(
    dependencies: NativeOfficialAccountFlowDependencies,
): NativeOfficialAccountFlow {
    return createNativeOfficialAccountFlowService(dependencies).flow
}

let configuredFlow: NativeOfficialAccountFlow | null = null

export function configureNativeOfficialAccountFlow(flow: NativeOfficialAccountFlow | null): void {
    configuredFlow = flow
}

export function getNativeOfficialAccountFlow(): NativeOfficialAccountFlow {
    if (!configuredFlow) throw new Error('Native official account flow is not configured')
    return configuredFlow
}
