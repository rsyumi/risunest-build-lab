// @vitest-environment jsdom

import { flushSync } from 'svelte'
import { expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'

vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))

vi.mock(import('../storage/database.svelte'), () => ({
    appVer: '1.0.0',
    getCurrentCharacter: () => ({ chaId: 'fixture-character', chatPage: 0, chats: [{ id: 'chat' }] }),
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
    processScriptFull: vi.fn(async (_char: unknown, data: string) => ({ data: `${data}{{raw::portrait}}`, emoChanged: false })),
} as unknown as typeof import('../process/scripts')))

import { ParseMarkdown, type simpleCharacterArgument } from './parser.svelte'
import { installDisplayRecovery } from '../displayRecovery.svelte'
import { recoveryStore } from '../displayRecovery.testSupport'

it.each([
    ['', false],
    [' while the record of its display work is being stored', true],
])('re-runs a reactive parse when an asset changes although only a display script adds its tag%s', async (_case, recording) => {
    const device = recording ? recoveryStore() : null
    if (device) await installDisplayRecovery(device.store)
    const assets = $state<[string, string, string][]>([['Portrait', 'portrait.png', 'png']])
    const character: simpleCharacterArgument = {
        type: 'simple',
        chaId: 'fixture-character',
        customscript: [],
        additionalAssets: assets,
        emotionImages: [],
    }
    let latest: Promise<string> | undefined
    const stop = $effect.root(() => {
        $effect(() => {
            latest = ParseMarkdown('Background ', character, 'back', 7)
        })
    })
    try {
        flushSync()
        device?.commitAll()
        expect(await latest).toBe('Background resolved:portrait.png')
        assets[0][1] = 'renamed.png'
        flushSync()
        expect(await latest).toBe('Background resolved:renamed.png')
    } finally {
        stop()
        await installDisplayRecovery(null)
    }
})
