import { language } from 'src/lang'
import { alertConfirm, alertError } from '../alert'
import { isTauri } from '../platform'
import { clearCharacterSelection } from 'src/lib/workingSetNavigation'
import { get } from 'svelte/store'
import { DBState, selectedCharID } from '../stores.svelte'
import { getPersistentDataStore } from './persistentDataStoreFactory'
import {
    getPersistentDataRuntime,
} from './persistentDataRuntime.svelte'
import type { ArchivePreview } from './persistentDataStore'
export {
    archivedAt,
    archivedConversationCount,
    characterIsArchived,
    countArchivedCharacters,
    countBlockedGroupMembers,
    formatArchivedAt,
} from './characterArchiveView'

export function archiveIsAvailable(): boolean {
    return isTauri
}

function formatCount(value: number): string {
    return value.toLocaleString()
}

/// Only a RisuAI account keeps no copy of an archived character, so the extra
/// warning belongs to that case alone.
async function onlyAccountBackupIsConnected(): Promise<boolean> {
    if (!DBState.db.account) return false
    if (!isTauri) return true
    try {
        const { getServerSyncController } = await import('./sync/serverSyncProduction')
        if (getServerSyncController().snapshot().status?.configured === true) return false
    } catch {}
    try {
        const { getExternalStorageBridge } = await import('./sync/external/bridge')
        const state = await getExternalStorageBridge().getState()
        if (state.connections.length > 0) return false
    } catch {}
    return true
}

async function buildArchiveConfirmation(preview: ArchivePreview): Promise<string> {
    const strings = language.risuNest.archive
    const lines = [strings.confirmBody]
    if (await onlyAccountBackupIsConnected()) {
        lines.push(strings.accountOnlyTitle)
        lines.push(strings.accountOnlyBody)
    }
    lines.push(
        strings.confirmCounts
            .replace('{0}', formatCount(preview.conversationCount))
            .replace('{1}', formatCount(preview.messageCount)),
    )
    return lines.join('\n\n')
}

function confirmWithTitle(title: string, body: string): Promise<boolean> {
    return alertConfirm(`${title}

${body}`)
}

async function runArchiveMutation(
    reason: string,
    mutate: (expectedRevision: number) => Promise<{ revision: number }>,
): Promise<boolean> {
    const runtime = getPersistentDataRuntime()
    const strings = language.risuNest.archive
    const restoring = reason === 'character-restore'
    let committed = false
    let fence: Awaited<ReturnType<typeof runtime.acquireDestructiveReplacementFence>> | undefined
    try {
        const token = await runtime.capturePersistentMutationToken(reason)
        fence = await runtime.acquireDestructiveReplacementFence(token)
        const applied = await mutate(token.revision)
        committed = true
        const outcome = await fence.refreshCommittedWorkingSet(applied.revision)
        if (outcome.projection !== 'applied') {
            alertError(restoring ? strings.restoreRefreshFailed : strings.archiveRefreshFailed)
        }
        return true
    } catch (error) {
        console.error('Character archive operation failed', error)
        alertError(committed
            ? restoring ? strings.restoreRefreshFailed : strings.archiveRefreshFailed
            : restoring ? strings.restoreFailed : strings.archiveFailed)
        return committed
    } finally {
        fence?.release()
    }
}

export async function archiveCharacterWithConfirmation(
    characterId: string,
    signal?: AbortSignal,
): Promise<boolean> {
    if (!archiveIsAvailable()) return false
    const store = getPersistentDataStore()
    let preview: ArchivePreview
    try {
        preview = await store.archivePreview(characterId)
    } catch (error) {
        console.error('Character archive preview failed', error)
        alertError(language.risuNest.archive.archiveFailed)
        return false
    }
    if (preview.archived) return false
    const strings = language.risuNest.archive
    const body = await buildArchiveConfirmation(preview)
    if (!await confirmWithTitle(strings.confirmTitle, body)) return false
    // An archived character cannot be open, so leave it before it is archived.
    const characters = DBState.db.characters
    if (characters[get(selectedCharID)]?.chaId === characterId && !await clearCharacterSelection()) return false
    return await runArchiveMutation('character-archive', (expectedRevision) =>
        store.archiveCharacter(characterId, expectedRevision, signal))
}

export async function restoreArchivedCharacterWithConfirmation(
    characterId: string,
    signal?: AbortSignal,
): Promise<boolean> {
    if (!archiveIsAvailable()) return false
    const store = getPersistentDataStore()
    const strings = language.risuNest.archive
    if (!await confirmWithTitle(strings.restoreTitle, strings.restoreBody)) return false
    return await runArchiveMutation('character-restore', (expectedRevision) =>
        store.restoreCharacter(characterId, expectedRevision, signal))
}
