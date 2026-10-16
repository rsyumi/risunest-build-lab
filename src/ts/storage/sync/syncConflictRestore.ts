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
import { hasLibraryCharacters } from '../libraryCharacters'
import { presentFileOperationError } from '../fileOperationErrorPresentation'
import type { NativeFileJobSource } from '../nativeFileJobs'
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
    if (isTauri) {
        const callStartedAt = Date.now()
        try {
            const { invoke } = await import('@tauri-apps/api/core')
            const { nativeRestoreDeadline } = await import('../nativeRestoreDeadline')
            const { restoreBackupFromNativeSource } = await import('../portableBackupFileRouteProduction.svelte')
            const { holdServerSyncAfterRestore } = await import('./serverSyncProduction')
            await restoreBackupFromNativeSource(async ({signal, onSource}) => {
                const source = await nativeRestoreDeadline(
                    invoke<NativeFileJobSource>('native_conflict_backup_source', { backupId: entry.id }),
                    'saved backup lookup', undefined,
                    source => invoke('native_portable_source_discard', {source}).then(() => {}), undefined, signal,
                )
                onSource({name: entryLabel(entry), bytes: entry.byteLength})
                return source
            }, {
                confirmationWarnings: [language.syncConflictRestoreScope],
                holdServerSyncAfterRestore: true,
                onNativeStatus(status) {
                    if (status.state === 'succeeded' && status.result) holdServerSyncAfterRestore()
                },
            })
        } catch (cause) {
            presentFileOperationError('import', cause, callStartedAt)
        }
        return
    }
    if (await hasLibraryCharacters() && !await alertConfirm(language.syncConflictRestoreConfirm)) return
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
    try {
        await flushPendingData('sync-conflict-restore')
        const current = await capturePersistentMutationToken('sync-conflict-restore')
        await installLocalBackup(decoded, {
            replaceDatabase: async (database, reason, options) => {
                const outcome = await replacePersistentDatabase(database, reason, {
                    ...options,
                    authoritative: true,
                    expectedRevision: current.revision,
                    expectedMutationGeneration: current.mutationGeneration,
                })
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
    }
}
