import { invoke } from '@tauri-apps/api/core'
import { isTauri } from '../platform'
import type { DataRevision } from './persistentDataStore'
import type { StorageMutationGate } from './storageMutationGate'

const OWNED_PREFIX = 'assets/'
/** The longest key, in UTF-8 bytes, the native cleanup accepts. */
const MAX_KEY_BYTES = 4096
const encoder = new TextEncoder()
/** A key inside longer text, such as an `<img src>` or a `{{raw::}}` reference. */
const EMBEDDED_KEY = /assets[\\/][^\s"'`<>()[\]{}|,;\\]+/g

function ownedKey(text: string): string | null {
    const key = text.replaceAll('\\', '/')
    // A UTF-16 length never exceeds the UTF-8 one, so it bounds the text before encoding.
    return key.startsWith(OWNED_PREFIX) && key.length > OWNED_PREFIX.length && key.length <= MAX_KEY_BYTES &&
        encoder.encode(key).length <= MAX_KEY_BYTES ? key : null
}

/** The `assets/` keys a character, module or persona names anywhere in its data. */
export function collectOwnedAssetKeys(...values: unknown[]): string[] {
    const keys = new Set<string>()
    const pending = [...values]
    while (pending.length) {
        const value = pending.pop()
        if (typeof value === 'string') {
            const whole = ownedKey(value)
            if (whole) keys.add(whole)
            for (const match of value.matchAll(EMBEDDED_KEY)) {
                const key = ownedKey(match[0])
                if (key) keys.add(key)
            }
        } else if (Array.isArray(value)) {
            pending.push(...value)
        } else if (value && typeof value === 'object') {
            pending.push(...Object.values(value))
        }
    }
    return [...keys]
}

/**
 * The `assets/` keys the conversations of characters about to be permanently deleted name, which
 * a read of their details leaves out. Empty on the web build and when the store cannot read them.
 */
export async function collectCharacterConversationAssetKeys(characterIds: readonly string[]): Promise<string[]> {
    if (!isTauri || characterIds.length === 0) return []
    try {
        return await invoke<string[]>('pds_character_asset_keys', { characterIds: [...characterIds] })
    } catch (error) {
        console.error('Deleted character conversation asset keys could not be read', error)
        return []
    }
}

/** The save queue and write gate an alias cleanup commits through. */
export interface AssetAliasCleanupTarget {
    runtime: {
        getStorageAuthorityEpoch(): number
        flushPendingData(reason: string): Promise<void>
        runStorageOnlyMutation(operation: (expectedRevision: DataRevision) => Promise<DataRevision>): Promise<void>
    }
    gate: Pick<StorageMutationGate, 'runWrite'>
}

/** The asset keys a permanently deleted owner named, and the wall-clock time its delete began. */
export interface DeletedOwnerAssets {
    keys: string[]
    startedAtMs: number
}

/**
 * Deletes the asset aliases nothing references, or with `owner` only those that owner named.
 * Pending edits are committed first so a reference that exists only in memory still counts, and
 * the save queue adopts the revision the delete commits. With `owner`, their files are deleted
 * in the background afterwards.
 */
export async function deleteUnusedAssetAliases(
    target: AssetAliasCleanupTarget,
    reason: string,
    owner?: DeletedOwnerAssets,
): Promise<void> {
    const { runtime, gate } = target
    const epoch = runtime.getStorageAuthorityEpoch()
    const flushed = runtime.flushPendingData(reason)
    // A failed flush rejects the cleanup below, so this copy only keeps it from going unhandled.
    flushed.catch(() => {})
    await runtime.runStorageOnlyMutation((expectedRevision) => gate.runWrite(async () => {
        await flushed
        if (epoch !== runtime.getStorageAuthorityEpoch()) throw new Error('Storage authority changed')
        return (await invoke<{ revision: DataRevision }>('pds_asset_delete_orphaned_aliases', {
            expectedRevision,
            owner: owner ?? null,
        })).revision
    }))
}

/**
 * After a permanent delete is committed, deletes the assets only the deleted owner used.
 * `startedAtMs` is the wall-clock time the delete began, so an asset saved since is kept.
 * Runs in the background; a failure leaves the assets to the unused-file cleanup.
 */
export function scheduleDeletedOwnerAssetCleanup(
    keys: readonly string[],
    startedAtMs: number,
    reason: string,
    target: () => AssetAliasCleanupTarget,
): void {
    if (!isTauri || keys.length === 0) return
    void Promise.resolve()
        .then(() => deleteUnusedAssetAliases(target(), reason, { keys: [...keys], startedAtMs }))
        .catch((error) => console.error('Deleted owner asset cleanup failed', error))
}
