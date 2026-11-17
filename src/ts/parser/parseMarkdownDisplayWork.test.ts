// @vitest-environment jsdom

import { expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'

vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))

vi.mock(import('../storage/database.svelte'), () => ({
    appVer: '1.0.0',
    getCurrentCharacter: () => ({ chaId: 'selected-character', chatPage: 0, chats: [{ id: 'selected-chat' }] }),
    getDatabase: () => ({}),
} as unknown as typeof import('../storage/database.svelte')))

vi.mock(import('../globalApi.svelte'), () => ({
    aiWatermarkingLawApplies: () => false,
    getFileSrc: async (path: string) => `resolved:${path}`,
}))

vi.mock(import('../stores.svelte'), () => ({
    DBState: {
        db: {
            assetWidth: -1,
            hideAllImages: false,
            legacyMediaFindings: false,
            assetMaxDifference: 1,
            characters: [],
            globalChatVariables: {},
            templateDefaultVariables: '',
        },
    },
    selIdState: { selId: -1 },
    selectedCharID: writable(-1),
} as unknown as typeof import('../stores.svelte')))

vi.mock(import('../process/modules'), () => ({
    getModuleAssets: () => [],
    getModuleLorebooks: () => [],
    getModules: () => [],
}))

vi.mock(import('../process/scripts'), () => ({
    processScriptFull: vi.fn(async (_char: unknown, data: string) => ({ data, emoChanged: false })),
} as unknown as typeof import('../process/scripts')))

import { ParseMarkdown, type simpleCharacterArgument } from './parser.svelte'
import { processScriptFull } from '../process/scripts'
import { installDisplayRecovery } from '../displayRecovery.svelte'
import { manualTimers, recoveryStore } from '../displayRecovery.testSupport'

function character(chaId: string): simpleCharacterArgument {
    return { type: 'simple', chaId, customscript: [], additionalAssets: [], emotionImages: [] }
}

it('stores one record for the rows of the selected chat before their display step and removes it after', async () => {
    const device = recoveryStore()
    const clock = manualTimers()
    await installDisplayRecovery(device.store, { timers: clock.timers })
    const selected = character('selected-character')
    try {
        const displayed = () => vi.mocked(processScriptFull).mock.calls.map(([, data]) => data)
        const rows = [0, 1, 2].map((index) => ParseMarkdown(`row ${index}`, selected, 'normal', index))
        // Text of another character is not display work of the chat, so it neither writes nor waits.
        await ParseMarkdown('preview', character('other-character'), 'normal', -1)
        expect(displayed()).toEqual(['preview'])
        expect(device.records).toEqual([{ working: [{ characterId: 'selected-character', conversationId: 'selected-chat' }], marked: [] }])

        device.commitAll()
        await Promise.all(rows)
        expect(displayed()).toEqual(['preview', 'row 0', 'row 1', 'row 2'])

        // A row loaded while the record is stored writes nothing.
        await ParseMarkdown('row 3', selected, 'normal', 3)
        expect(device.records).toHaveLength(1)

        clock.runAll()
        expect(device.records.at(-1)).toBeNull()
    } finally {
        await installDisplayRecovery(null)
    }
})
