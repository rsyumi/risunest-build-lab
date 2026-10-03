import { language } from 'src/lang'
import { isTauri } from '../../platform'
import { alertConfirm, alertError, alertNormal, alertSelect } from '../../alert'
import type { Database } from '../database.svelte'
import { installLocalBackup } from '../databaseRestore'
import {
    flushPendingData,
    capturePersistentMutationToken,
    publishCurrentOfficialRevision,
    replacePersistentDatabase,
} from '../persistentDataRuntime.svelte'
import { decodeRisuSave } from '../risuSave'
import {
    getSyncConflictBackupStore,
    type SyncConflictBackupEntry,
} from './syncConflictBackup'

function entryLabel(entry: SyncConflictBackupEntry): string {
    const side = entry.side === 'local'
        ? language.syncBackupSideLocal
        : language.syncBackupSideRemote
    const label = language.syncBackupEntry
        .replace('{date}', new Date(entry.createdAt).toLocaleString())
        .replace('{side}', side)
        .replace('{count}', `${entry.characterCount}`)
    return `${label} / ${language.syncBackupDatabaseOnly}`
}

export async function openSyncConflictBackups(): Promise<void> {
    const store = getSyncConflictBackupStore()
    const entries = await store.list()
    if (entries.length === 0) {
        alertNormal(language.syncConflictNoBackups)
        return
    }
    const selected = await alertSelect(entries.map(entryLabel), language.syncConflictBackups)
    const entry = entries[Number(selected)]
    if (!entry) return
    // Native replacement asks once in its own restore dialog, after the sync pull.
    if (!isTauri && !await alertConfirm(language.syncConflictRestoreConfirm)) return
    let decoded: Database
    try {
        const bytes = await store.read(entry.id)
        if (!bytes) throw new Error('conflict-backup-missing')
        decoded = await decodeRisuSave(bytes) as Database
        if (!decoded || typeof decoded !== 'object' || !Array.isArray(decoded.characters)) {
            throw new Error('conflict-backup-invalid')
        }
    } catch (cause) {
        console.error('Conflict backup could not be read', cause)
        alertError(language.syncConflictBackupUnreadable)
        return
    }
    let release: (() => void | Promise<void>) | undefined
    let hold: (() => void) | undefined
    try {
        if (isTauri) {
            const { getServerSyncController, holdServerSyncAfterRestore } = await import('./serverSyncProduction')
            const controller = getServerSyncController()
            release = await controller.beginReplacement()
            await controller.confirmReplacement()
            hold = holdServerSyncAfterRestore
        }
        await flushPendingData('sync-conflict-restore')
        // The native replacement pins the revision it pauses at, which follows the sync pull.
        const current = isTauri ? undefined : await capturePersistentMutationToken('sync-conflict-restore')
        await installLocalBackup(decoded, {
            ...(isTauri ? { upstreamImportWarnings: [language.syncConflictRestoreScope] } : {}),
            replaceDatabase: async (database, reason, options) => {
                const outcome = await replacePersistentDatabase(database, reason, {
                    ...options,
                    authoritative: true,
                    ...(current ? {
                        expectedRevision: current.revision,
                        expectedMutationGeneration: current.mutationGeneration,
                    } : {}),
                })
                hold?.()
                return outcome
            },
            publishAcceptedRevision: publishCurrentOfficialRevision,
            onPostCommitError: (error) => {
                console.error('Committed conflict restore follow-up failed', error)
                alertError(language.risuNest.persistentData.followupFailed)
            },
            relaunch: () => location.reload(),
        })
    } catch (cause) {
        if (cause instanceof DOMException && cause.name === 'AbortError') return
        console.error('Conflict backup restoration failed', cause)
        const syncUnavailable = typeof cause === 'object' && cause !== null && 'code' in cause && cause.code === 'sync-unavailable'
        alertError(syncUnavailable ? language.risuNest.backup.syncUnavailable : language.risuNest.backup.actionFailed)
    } finally {
        await release?.()
    }
}
