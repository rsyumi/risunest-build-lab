import { describe, expect, it, vi } from 'vitest'
import type { PersistentRevisionReader } from '../persistentDataStore'
import { encodeLogicalRecordKey as key } from './logicalRecordKey'
import { summarizeServerSyncConflict } from './serverSyncConflictSummary'

describe('server conflict identities', () => {
    it('identifies settings, presets, unloaded characters and deleted chats at the preview revision', async () => {
        const reader = {
            revision: 7,
            readCharacterSummary: vi.fn().mockResolvedValue({ name: 'Unloaded character' }),
            readConversationMetadata: vi.fn().mockResolvedValue(null),
        } as unknown as PersistentRevisionReader
        const summary = await summarizeServerSyncConflict({ localRevision: 7, conflictCount: 5, conflicts: [
            key({ kind: 'root' }), key({ kind: 'preset', presetId: 'preset-id' }),
            key({ kind: 'character', characterId: 'unloaded' }),
            key({ kind: 'conversation', characterId: 'unloaded', conversationId: 'deleted-chat' }),
            'plugin-storage:synthetic-plugin',
        ] }, reader)
        expect(summary.groups.map((group) => [group.kind, group.names])).toEqual([
            ['root', ['']], ['preset', ['preset-id']], ['character', ['Unloaded character']],
            ['conversation', ['unloaded / deleted-chat']], ['plugin', ['synthetic-plugin']],
        ])
        expect(reader.readConversationMetadata).toHaveBeenCalledWith('unloaded', 'deleted-chat')
    })
    it('bounds name lookups and preserves the total beyond the server preview limit', async () => {
        const reader = { revision: 4, readCharacterSummary: vi.fn().mockResolvedValue(null), readConversationMetadata: vi.fn() }
        const summary = await summarizeServerSyncConflict({ localRevision: 4, conflictCount: 123,
            conflicts: Array.from({ length: 100 }, (_, i) => key({ kind: 'character', characterId: `id-${i}` })),
        }, reader)
        expect(reader.readCharacterSummary).toHaveBeenCalledTimes(3)
        expect(summary).toEqual({ groups: [{ kind: 'character', names: ['id-0', 'id-1', 'id-2'], count: 100 }], remaining: 23 })
    })
    it('never resolves names from a different revision', async () => {
        const reader = { revision: 5, readCharacterSummary: vi.fn(), readConversationMetadata: vi.fn() }
        const summary = await summarizeServerSyncConflict({ localRevision: 4, conflictCount: 1,
            conflicts: [key({ kind: 'character', characterId: 'stable-id' })],
        }, reader)
        expect(reader.readCharacterSummary).not.toHaveBeenCalled()
        expect(summary.groups[0].names).toEqual(['stable-id'])
    })
})
