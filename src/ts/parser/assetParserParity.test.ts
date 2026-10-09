// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'

vi.mock('../platform', () => ({ get isTauri() { return parserMocks.native }, isNodeServer: false }))

const parserMocks = vi.hoisted(() => ({
    native: false,
    characters: [] as unknown[],
    selIdState: { selId: -1 },
    getModuleAssets: vi.fn((): [string, string, string][] => [['Theme', 'theme.mp3', 'mp3']]),
    getCurrentCharacter: vi.fn(() => ({ image: 'live.png' })),
    getFileSrc: vi.fn((path: string) => Promise.resolve(`resolved:${path}`)),
    getFileImageSource: vi.fn(async (): Promise<import('../storage/blobStore').BlobImageSource | null> => null),
}))

vi.mock(import('../storage/database.svelte'), () => ({
    appVer: '1.0.0',
    getCurrentCharacter: parserMocks.getCurrentCharacter,
    getDatabase: () => ({}),
} as unknown as typeof import('../storage/database.svelte')))

vi.mock(import('../globalApi.svelte'), () => ({
    aiWatermarkingLawApplies: () => false,
    getFileImageSource: parserMocks.getFileImageSource,
    getFileSrc: parserMocks.getFileSrc,
}))

vi.mock(import('../stores.svelte'), () => ({
    DBState: {
        db: {
            assetWidth: -1,
            hideAllImages: false,
            legacyMediaFindings: false,
            assetMaxDifference: 1,
            characters: parserMocks.characters,
            globalChatVariables: {},
            templateDefaultVariables: '',
        },
    },
    selIdState: parserMocks.selIdState,
    selectedCharID: writable(-1),
} as unknown as typeof import('../stores.svelte')))

vi.mock(import('../process/modules'), () => ({
    getModuleAssets: parserMocks.getModuleAssets,
    getModuleLorebooks: () => [],
    getModules: () => [],
}))

vi.mock(import('../process/scripts'), () => ({
    processScriptFull: vi.fn((_char: unknown, data: string) => Promise.resolve({ data, emoChanged: false })),
} as unknown as typeof import('../process/scripts')))

import { ParseMarkdown, resetAssetsCache, type simpleCharacterArgument } from './parser.svelte'
import { processScriptFull } from '../process/scripts'

const character: simpleCharacterArgument = {
    type: 'simple',
    chaId: 'fixture-character',
    customscript: [],
    additionalAssets: [
        ['Portrait', 'portrait.png', 'png'],
        ['happy-face.png', 'happy.png', 'png'],
    ],
    emotionImages: [['Smile', 'smile.png']],
}

describe('parser asset resolution parity', () => {
    afterEach(() => {
        parserMocks.native = false
        parserMocks.getFileImageSource.mockReset()
    })

    it.each(['img::portrait', 'image::portrait', 'emotion::smile'])('emits stored intrinsic dimensions for %s before image loading', async (token) => {
        parserMocks.native = true
        const recordDimensions = vi.fn(async () => {})
        parserMocks.getFileImageSource.mockResolvedValue({
            url: 'https://synthetic.invalid/image.png',
            contentHash: 'a'.repeat(64),
            metadata: { kind: 'asset', key: 'assets/image.png', mime: 'image/png', name: 'image.png', ext: 'png', size: 1 },
            width: 640,
            height: 480,
            recordDimensions,
        })
        const result = await ParseMarkdown(`{{${token}}}`, character, 'back', 7)
        const root = document.createElement('div')
        root.innerHTML = result
        const image = root.querySelector('img')!
        expect(image.getAttribute('src')).toBe('https://synthetic.invalid/image.png')
        expect(image.getAttribute('width')).toBe('640')
        expect(image.getAttribute('height')).toBe('480')
        expect(image.getAttribute('loading')).toBe('lazy')
        expect(recordDimensions).not.toHaveBeenCalled()
    })

    it('does not run live scripts when navigation aborts after assets resolve', async () => {
        const controller = new AbortController()
        let resolveAsset!: (value: string) => void
        parserMocks.getFileSrc.mockImplementationOnce(
            () =>
                new Promise((resolve) => {
                    resolveAsset = resolve
                }),
        )
        vi.mocked(processScriptFull).mockClear()
        const pending = ParseMarkdown(
            '{{raw::portrait}}',
            {
                ...character,
                chaId: 'navigation-abort-character',
                additionalAssets: [['Portrait', 'navigation-abort.png', 'png']],
            },
            'back',
            -1,
            {},
            { signal: controller.signal },
        )
        await vi.waitFor(() => expect(resolveAsset).toBeTypeOf('function'))
        // Navigation publishes before the queued asset continuation gets its turn.
        resolveAsset('resolved:navigation-abort.png')
        controller.abort()
        await expect(pending).rejects.toMatchObject({ name: 'AbortError' })
        expect(processScriptFull).not.toHaveBeenCalled()
    })

    it('keeps exact, fuzzy, module, emotion, and missing parser results', async () => {
        const result = await ParseMarkdown(
            '{{raw::PORTRAIT}}|{{path::happy_face}}|{{raw::THEME}}|{{emotion::SMILE}}|{{raw::missing}}',
            character,
            'back',
            7,
        )

        expect(result).toBe([
            'resolved:portrait.png',
            'resolved:happy.png',
            'resolved:theme.mp3',
            '<img alt="resolved:smile.png" style="" loading="lazy" decoding="async">',
            '',
        ].join('|'))
    })

    it('does not retain an asset index from a previous simple character argument', async () => {
        const otherCharacter: simpleCharacterArgument = {
            ...character,
            chaId: 'other-character',
            additionalAssets: [['Portrait', 'other.png', 'png']],
            emotionImages: [],
        }

        expect(await ParseMarkdown('{{raw::portrait}}', character, 'back', 7)).toBe('resolved:portrait.png')
        expect(await ParseMarkdown('{{raw::portrait}}', otherCharacter, 'back', 7)).toBe('resolved:other.png')
        expect(await ParseMarkdown('{{raw::portrait}}', character, 'back', 7)).toBe('resolved:portrait.png')
    })

    it('reuses the selected owner index for its simple character view', async () => {
        parserMocks.characters[0] = {
            type: 'character',
            chaId: character.chaId,
            additionalAssets: character.additionalAssets,
            emotionImages: character.emotionImages,
        }
        parserMocks.selIdState.selId = 0
        resetAssetsCache(
            character.additionalAssets ?? [],
            character.emotionImages ?? [],
            [['Theme', 'theme.mp3', 'mp3']],
            `0:${character.chaId}`,
        )
        parserMocks.getModuleAssets.mockClear()

        expect(await ParseMarkdown('{{raw::portrait}}', character, 'back', 7)).toBe('resolved:portrait.png')
        expect(parserMocks.getModuleAssets).not.toHaveBeenCalled()

        parserMocks.characters.length = 0
        parserMocks.selIdState.selId = -1
    })

    it('uses frozen module and source assets without selected-character lookups', async () => {
        parserMocks.getCurrentCharacter.mockClear()
        parserMocks.getModuleAssets.mockClear()
        parserMocks.getFileSrc.mockClear()

        const result = await ParseMarkdown(
            '{{source::char}}|{{source::user}}|{{raw::capture-module}}',
            character,
            'back',
            7,
            {},
            {
                moduleAssets: [['capture-module', 'capture.png', 'png']],
                characterImageSource: 'frozen-char.png',
                userImageSource: 'frozen-user.png',
                assetWidth: -1,
                hideAllImages: false,
                legacyMediaFindings: false,
                assetMaxDifference: 1,
            },
        )

        expect(result).toBe('resolved:frozen-char.png|resolved:frozen-user.png|resolved:capture.png')
        expect(parserMocks.getCurrentCharacter).not.toHaveBeenCalled()
        expect(parserMocks.getModuleAssets).not.toHaveBeenCalled()
    })
})
