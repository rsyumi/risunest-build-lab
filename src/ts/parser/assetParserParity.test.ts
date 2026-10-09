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

import { ParseMarkdown, resetAssetsCache, trimMarkdown, type simpleCharacterArgument } from './parser.svelte'
import { processScriptFull } from '../process/scripts'
import { DeferredInlayMarkerRegistry, mountDeferredInlaySources } from '../process/files/inlayRenderSource'

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
        parserMocks.getFileSrc.mockReset().mockImplementation(async (path: string) => `resolved:${path}`)
        document.body.replaceChildren()
    })

    it.each([
        '<img class="han-rounded-image" src="{{raw::portrait}}">',
        '<img src="{{path::portrait}}">',
        '{{asset::portrait}}',
        '<img src="{{source::char}}">',
        '<img src="{{source::user}}">',
        '![portrait]({{raw::portrait}})',
    ])('reserves known local image geometry before mounting %s', async (input) => {
        parserMocks.native = true
        parserMocks.getFileSrc.mockResolvedValue('https://synthetic.invalid/image.png')
        parserMocks.getFileImageSource.mockResolvedValue({
            url: 'https://synthetic.invalid/image.png', contentHash: 'a'.repeat(64),
            metadata: { kind: 'asset', key: 'assets/image.png', mime: 'image/png', name: 'image.png', ext: 'png', size: 1 },
            width: 640, height: 480, recordDimensions: vi.fn(async () => {}),
        })
        const result = await ParseMarkdown(input, character, 'normal', 7, {}, {
            characterImageSource: 'assets/character.png', userImageSource: 'assets/user.png',
        })
        const root = document.createElement('template')
        root.innerHTML = result
        const image = root.content.querySelector('img')!
        expect(image.getAttribute('src')).toBe('https://synthetic.invalid/image.png')
        expect(image.getAttribute('width')).toBe('640')
        expect(image.getAttribute('height')).toBe('480')
        expect(image.getAttribute('loading')).toBe('lazy')
    })

    it('learns unknown raw image geometry on load without changing raw URLs in CSS or text', async () => {
        parserMocks.native = true
        const recordDimensions = vi.fn(async () => {})
        const url = 'https://synthetic.invalid/image.png'
        parserMocks.getFileSrc.mockResolvedValue(url)
        parserMocks.getFileImageSource.mockResolvedValue({
            url, contentHash: 'b'.repeat(64), recordDimensions,
            metadata: { kind: 'asset', key: 'assets/image.png', mime: 'image/png', name: 'image.png', ext: 'png', size: 1 },
        })
        const registry = new DeferredInlayMarkerRegistry()
        const root = document.createElement('div')
        root.innerHTML = await ParseMarkdown('<img src="{{raw::portrait}}"><div style="background-image:url({{raw::portrait}})">{{path::portrait}}</div>', character, 'back', 7, {}, { deferredInlays: registry })
        document.body.append(root)
        const image = root.querySelector('img')!
        expect(image.hasAttribute('width')).toBe(false)
        expect(root.querySelector('div')!.style.backgroundImage).toContain(url)
        expect(root.textContent).toBe(url)
        const cleanup = mountDeferredInlaySources(root, registry)
        Object.defineProperties(image, { naturalWidth: { value: 800 }, naturalHeight: { value: 600 } })
        image.dispatchEvent(new Event('load'))
        expect(recordDimensions).toHaveBeenCalledExactlyOnceWith(800, 600)
        cleanup()
    })

    it('sizes raw images introduced by editdisplay when notrim output is finalized', async () => {
        parserMocks.native = true
        parserMocks.getFileImageSource.mockResolvedValue({
            url: 'https://synthetic.invalid/image.png', contentHash: 'c'.repeat(64),
            metadata: { kind: 'asset', key: 'assets/image.png', mime: 'image/png', name: 'image.png', ext: 'png', size: 1 },
            width: 900, height: 600, recordDimensions: vi.fn(async () => {}),
        })
        vi.mocked(processScriptFull).mockResolvedValueOnce({ data: '<style>.han-rounded-image{border-radius:8px}</style><img class="han-rounded-image" src="{{raw::portrait}}">', emoChanged: false })
        const registry = new DeferredInlayMarkerRegistry()
        const parsed = await ParseMarkdown('synthetic regex input', character, 'notrim', 7, {}, { deferredInlays: registry })
        const root = document.createElement('template')
        root.innerHTML = trimMarkdown(parsed, { deferredInlays: registry })
        expect(root.content.querySelector('img')?.outerHTML).toContain('width="900" height="600"')
        expect(root.content.querySelector('img')?.className).toBe('x-risu-han-rounded-image')
        expect(root.content.querySelector('style')?.textContent).toContain('border-radius:8px')
    })

    it.each([
        ['width="120"', '120', null],
        ['height="80"', null, '80'],
        ['style="width:100%;height:auto"', null, null],
        ['srcset="https://synthetic.invalid/other.png 2x"', null, null],
    ])('preserves authored raw image sizing: %s', async (attributes, width, height) => {
        parserMocks.native = true
        parserMocks.getFileImageSource.mockResolvedValue({
            url: 'https://synthetic.invalid/image.png', contentHash: 'c'.repeat(64),
            metadata: { kind: 'asset', key: 'assets/image.png', mime: 'image/png', name: 'image.png', ext: 'png', size: 1 },
            width: 640, height: 480, recordDimensions: vi.fn(async () => {}),
        })
        const root = document.createElement('template')
        root.innerHTML = await ParseMarkdown(`<img src="{{raw::portrait}}" ${attributes}>`, character, 'back')
        const image = root.content.querySelector('img')!
        expect(image.getAttribute('width')).toBe(width)
        expect(image.getAttribute('height')).toBe(height)
        if (image.hasAttribute('srcset')) expect(image.hasAttribute('data-risu-inlay-slot')).toBe(false)
    })

    it('preserves sizing added by editdisplay to a generated asset image', async () => {
        parserMocks.native = true
        parserMocks.getFileImageSource.mockResolvedValue({
            url: 'https://synthetic.invalid/image.png', contentHash: 'c'.repeat(64),
            metadata: { kind: 'asset', key: 'assets/image.png', mime: 'image/png', name: 'image.png', ext: 'png', size: 1 },
            width: 640, height: 480, recordDimensions: vi.fn(async () => {}),
        })
        vi.mocked(processScriptFull).mockImplementationOnce(async (_char, data) => ({ data: data.replace('<img ', '<img width="120" '), emoChanged: false }))
        const root = document.createElement('template')
        root.innerHTML = await ParseMarkdown('{{img::portrait}}', character, 'back')
        expect(root.content.querySelector('img')?.getAttribute('width')).toBe('120')
        expect(root.content.querySelector('img')?.hasAttribute('height')).toBe(false)
    })

    it('leaves responsive picture, external images, and non-image raw substitutions alone', async () => {
        parserMocks.native = true
        parserMocks.getFileImageSource.mockResolvedValue({
            url: 'https://synthetic.invalid/image.png', contentHash: 'c'.repeat(64),
            metadata: { kind: 'asset', key: 'assets/image.png', mime: 'image/png', name: 'image.png', ext: 'png', size: 1 },
            width: 640, height: 480, recordDimensions: vi.fn(async () => {}),
        })
        const root = document.createElement('template')
        root.innerHTML = await ParseMarkdown('<picture><source srcset="https://synthetic.invalid/other.png"><img src="{{raw::portrait}}"></picture><picture><source srcset="https://synthetic.invalid/other.png">{{img::portrait}}</picture><img src="https://external.invalid/image.png"><a href="{{raw::portrait}}">link</a>', character, 'back')
        for (const image of root.content.querySelectorAll('img')) {
            expect(image.hasAttribute('width')).toBe(false)
            expect(image.hasAttribute('data-risu-inlay-slot')).toBe(false)
        }
        expect(root.content.querySelector('a')?.getAttribute('href')).toBe('https://synthetic.invalid/image.png')
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
