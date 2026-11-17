import { describe, expect, it, vi } from 'vitest'

vi.mock('../util', () => ({
    checkNullish: (value: unknown) => value === null || value === undefined,
    decryptBuffer: vi.fn(),
    encryptBuffer: vi.fn(),
    selectSingleFile: vi.fn(),
}))
vi.mock('../alert', () => ({ alertNormal: vi.fn() }))
vi.mock('../gui/colorscheme', () => ({ defaultColorScheme: {} }))
vi.mock('../translator/presets', () => ({
    normalizeTranslatorPresetState: vi.fn(),
}))
vi.mock('../stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return {
        DBState: { db: { characters: [] } },
        selectedCharID: writable(-1),
        selIdState: { selId: -1 },
    }
})
vi.mock('../model/modellist', () => ({
    LLMFlags: {},
    LLMFormat: { OpenAICompatible: 'openai-compatible' },
    LLMTokenizer: {},
}))
import { normalizeDatabaseDefaults, deriveEffectivePresetMirrors, flushEffectivePresetEdits, getCharacterByIndex, getCurrentCharacter, type Database } from './database.svelte'
import { DBState, selectedCharID } from '../stores.svelte'

it('derives missing preset fields from current defaults instead of another preset mirror', () => {
    const database = normalizeDatabaseDefaults({ characters: [] } as Database)
    database.botPresets = [{ id: 'a', name: 'A', mainPrompt: 'A' }, { id: 'b', name: 'B' }] as Database['botPresets']
    database.botPresetsId = 0
    deriveEffectivePresetMirrors(database)
    expect(database.mainPrompt).toBe('A')
    database.botPresetsId = 1
    deriveEffectivePresetMirrors(database)
    expect(database.mainPrompt).toBe(normalizeDatabaseDefaults({ characters: [] } as Database).mainPrompt)
    flushEffectivePresetEdits(database)
    expect(database.botPresets[1]).toEqual({ id: 'b', name: 'B' })
})

it('creates IDs for new defaults without adding IDs to existing records during normalization', () => {
    const fresh = normalizeDatabaseDefaults({ characters: [] } as Database)
    expect(fresh.botPresets[0].id).toMatch(/^[0-9a-f-]{36}$/)
    expect(fresh.personas[0].id).toMatch(/^[0-9a-f-]{36}$/)
    const existing = normalizeDatabaseDefaults({ characters: [], botPresets: [{ name: 'Existing' }], personas: [{ name: 'Existing' }] } as Database)
    expect(existing.botPresets[0].id).toBeUndefined()
    expect(existing.personas[0].id).toBeUndefined()
})

it('snapshots only the requested character', () => {
    const characters = [{ chaId: 'first', name: 'First' }, { chaId: 'second', name: 'Second', chats: [{ message: [] }] }]
    const database = { characters } as unknown as Database
    Object.defineProperty(database, 'unrelated', { enumerable: true, get: () => { throw new Error('read outside the character') } })
    DBState.db = database
    selectedCharID.set(1)
    for (const snapshot of [getCharacterByIndex(1, { snapshot: true }), getCurrentCharacter({ snapshot: true })]) {
        expect(snapshot).toEqual(characters[1])
        expect(snapshot).not.toBe(characters[1])
        snapshot.name = 'Edited'
        expect(characters[1].name).toBe('Second')
    }
    expect(getCharacterByIndex(1)).toBe(characters[1])
})

describe('streaming display defaults', () => {
    it('enables compact thoughts by default and preserves an explicit opt-out', () => {
        expect(
            normalizeDatabaseDefaults({ characters: [] } as Database)
                .streamingThoughtMode,
        ).toBe('recent')
        expect(
            normalizeDatabaseDefaults({
                characters: [],
                streamingThoughtMode: 'off',
            } as Database).streamingThoughtMode,
        ).toBe('off')
        expect(
            normalizeDatabaseDefaults({ characters: [] } as Database)
                .streamingDeferDisplayProcessing,
        ).toBe(false)
        expect(
            normalizeDatabaseDefaults({
                characters: [],
                streamingDeferDisplayProcessing: true,
            } as Database).streamingDeferDisplayProcessing,
        ).toBe(true)
    })

    it('edits chat messages in the popup by default and preserves the inline choice', () => {
        expect(
            normalizeDatabaseDefaults({ characters: [] } as Database)
                .risunestChatEditPopup,
        ).toBe(true)
        expect(
            normalizeDatabaseDefaults({
                characters: [],
                risunestChatEditPopup: false,
            } as Database).risunestChatEditPopup,
        ).toBe(false)
    })

    it.each(['risunestSyncStatusUi', 'risunestScrollNavigation'] as const)('enables %s by default and preserves turning it off', (key) => {
        expect(normalizeDatabaseDefaults({ characters: [] } as Database)[key]).toBe(true)
        const database = { characters: [] } as Database
        database[key] = false
        expect(normalizeDatabaseDefaults(database)[key]).toBe(false)
    })
})

describe('RisuNest inlay database defaults', () => {
    it('normalizes the persisted inlay settings to their exact defaults', () => {
        const database = normalizeDatabaseDefaults({
            characters: [],
        } as Database)

        expect(database.risunestInlayFormat).toBe('webp')
        expect(database.risunestInlayWebpQuality).toBe(85)
        expect(database.risunestInlayMaxDimension).toBe(0)
        expect(database.risunestInlaySkipReencode).toBe(true)
    })

    it('clamps and integer-normalizes persisted inlay numbers', () => {
        const database = normalizeDatabaseDefaults({
            characters: [],
            risunestInlayWebpQuality: 101.6,
            risunestInlayMaxDimension: -2.4,
        } as Database)

        expect(database.risunestInlayWebpQuality).toBe(100)
        expect(database.risunestInlayMaxDimension).toBe(0)
    })

    it.each([
        [Number.NaN, 0],
        [-1, 0],
        [12.6, 13],
        [4_294_967_296, 4_294_967_295],
        [Number.MAX_SAFE_INTEGER, 4_294_967_295],
    ])(
        'normalizes persisted maximum dimension %s into the native u32 range',
        (input, expected) => {
            const database = normalizeDatabaseDefaults({
                characters: [],
                risunestInlayMaxDimension: input,
            } as Database)

            expect(database.risunestInlayMaxDimension).toBe(expected)
        },
    )
})
