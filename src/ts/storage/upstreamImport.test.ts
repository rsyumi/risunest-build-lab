import { describe, expect, it } from 'vitest'
import type { Database } from './database.svelte'
import { exportMessageNameSettings, importMessageNameSettings, prepareExternalDatabaseImport } from './upstreamImport'

describe('external database import', () => {
    it('imports ordinary characters and prunes discarded upstream records from folders and loadouts', () => {
        const character = { type: 'character', chaId: 'member', name: 'Synthetic', chats: [{ message: [{ role: 'user', data: 'Hello' }] }] }
        const database = {
            characters: [character, { type: 'group', chaId: 'group', characters: ['member'] }, { type: 'character', chaId: '§temp' }],
            characterOrder: ['group', 'member', '§temp', { id: 'folder', data: ['group', 'member'] }, { id: 'empty', data: ['group'] }],
            loadouts: [{ characterIds: ['member', 'group', '§temp'] }],
            groupTemplate: '{{char}}: {{slot}}',
            botPresets: [{ groupOtherBotRole: 'system', promptSettings: { sendName: true } }],
            protectedPresetValues: { groupTemplate: 'Protected {{slot}}' },
        } as unknown as Database

        const imported = prepareExternalDatabaseImport(database)
        expect(imported.characters).toEqual([character])
        expect(imported.characterOrder).toEqual(['member', { id: 'folder', data: ['member'] }])
        expect(imported.loadouts).toEqual([{ characterIds: ['member'] }])
        expect(imported.messageNameTemplate).toBe('{{char}}: {{slot}}')
        expect(imported.botPresets).toEqual([{ namedMessageRole: 'system', promptSettings: { sendName: true } }])
        expect(imported.protectedPresetValues).toEqual({ messageNameTemplate: 'Protected {{slot}}' })
    })

    it('converts upstream preset names after applying preset defaults', () => {
        const preset = { messageNameTemplate: '', namedMessageRole: 'user', groupTemplate: '{{slot}}', groupOtherBotRole: 'assistant' }
        importMessageNameSettings(preset)
        expect(preset).toEqual({ messageNameTemplate: '{{slot}}', namedMessageRole: 'assistant' })
    })

    it('round-trips message name settings through the upstream preset format', () => {
        const preset = { messageNameTemplate: '[{{char}}] {{slot}}', namedMessageRole: 'system', promptSettings: { sendName: true } }
        const exported = exportMessageNameSettings(preset)
        expect(exported).toEqual({ groupTemplate: '[{{char}}] {{slot}}', groupOtherBotRole: 'system', promptSettings: { sendName: true } })
        importMessageNameSettings(exported)
        expect(exported).toEqual(preset)
        expect(preset.messageNameTemplate).toBe('[{{char}}] {{slot}}')
    })

})
