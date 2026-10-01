import type { PersistentRevisionReader } from '../persistentDataStore'
import { decodeLogicalRecordKey, type LogicalRecordLocator } from './logicalRecordKey'
import type { ServerCycle } from './serverSync'

export type ConflictKind = LogicalRecordLocator['kind'] | 'unknown'
export interface ServerConflictGroup { kind: ConflictKind; names: string[]; count: number }
export interface ServerConflictSummary { groups: ServerConflictGroup[]; remaining: number }
const short = (value: string) => value.length > 48 ? `${value.slice(0, 45)}…` : value

export async function summarizeServerSyncConflict(
    preview: Pick<ServerCycle, 'conflicts' | 'conflictCount' | 'localRevision'>,
    reader?: Pick<PersistentRevisionReader, 'revision' | 'readCharacterSummary' | 'readConversationMetadata'>,
): Promise<ServerConflictSummary> {
    const groups = new Map<ConflictKind, ServerConflictGroup>()
    const keys = preview.conflicts.slice(0, 100)
    for (const key of keys) {
        let locator: LogicalRecordLocator | undefined
        let kind: ConflictKind = 'unknown'
        let name = short(key)
        try {
            if (key.startsWith('plugin-storage:')) {
                kind = 'plugin'
                name = short(key.slice('plugin-storage:'.length))
            } else {
                locator = decodeLogicalRecordKey(key)
                kind = locator.kind
                if ('characterId' in locator) name = short(locator.characterId)
                if ('conversationId' in locator) name += ` / ${short(locator.conversationId)}`
                if ('presetId' in locator) name = short(locator.presetId)
                if ('owner' in locator) name = short(locator.owner)
                if ('logicalKey' in locator) name = short(locator.logicalKey)
                if (kind === 'root') name = ''
            }
        } catch { /* Unknown keys retain their bounded identifier. */ }
        const group: ServerConflictGroup = groups.get(kind) ?? { kind, names: [], count: 0 }
        groups.set(kind, group)
        group.count++
        if (group.names.length >= 3) continue
        if (reader?.revision === preview.localRevision && locator) {
            try {
                if (locator.kind === 'character') {
                    const summary = await reader.readCharacterSummary(locator.characterId)
                    if (summary?.name) name = short(summary.name)
                } else if (locator.kind === 'conversation') {
                    const metadata = await reader.readConversationMetadata(locator.characterId, locator.conversationId)
                    if (metadata?.revision === reader.revision && metadata.value.conversation.name) name = short(metadata.value.conversation.name)
                }
            } catch { /* Deleted, archived and unavailable records keep their identifiers. */ }
        }
        group.names.push(name)
    }
    return { groups: [...groups.values()], remaining: Math.max(0, preview.conflictCount - keys.length) }
}
