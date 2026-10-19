import { describe, expect, it, vi } from 'vitest'
import { imageFixture } from '../../../tests/support/imageFixtures'

import {
    archiveCurrentCharacterImage,
    selectCharacterImageFile,
} from './characterImageFileRoute'

describe('character image file route', () => {
    it.each([['png', 'photo.jpg'], ['jpg', 'photo.png']] as const)('routes %s content named %s by its bytes', async (ext, name) => {
        const bytes = imageFixture(ext)
        const nativeImport = vi.fn()
        const result = await selectCharacterImageFile('character-1', {
            isTauriDesktop: true,
            chooseDesktopPath: async () => `/chosen/${name}`,
            readDesktopHeader: async () => bytes.slice(0, 3),
            readDesktopPath: async () => bytes,
            chooseLegacyFile: async () => null,
            nativeImport,
            legacyImport: async data => (await import('../media/imageFormats')).requireImage(data).ext,
        })
        expect(result).toMatchObject({ kind: 'selected', logicalId: ext })
        expect(nativeImport).not.toHaveBeenCalled()
    })

    it.each([
        ['assets/first.JPG', 'jpg'],
        ['assets/second.JPEG?download=1#portrait', 'jpeg'],
    ])('preserves the previous JPEG extension when replacing %s', (uri, extension) => {
        const character = { image: uri, ccAssets: [] }

        archiveCurrentCharacterImage(character)

        expect(character).toEqual({
            image: '',
            ccAssets: [{
                type: 'icon',
                name: 'iconx',
                uri,
                ext: extension,
            }],
        })
    })

    it('keeps desktop JPEG bytes out of JavaScript and uses the explicit native destination', async () => {
        const nativeImport = vi.fn(async () => ({
            revision: 8,
            logicalId: 'assets/hash.jpeg',
        }))
        const readDesktopPath = vi.fn(async () => Uint8Array.from([1, 2, 3]))
        const legacyImport = vi.fn(async () => 'legacy')

        const result = await selectCharacterImageFile('character-1', {
            isTauriDesktop: true,
            chooseDesktopPath: vi.fn(async () => 'C:\\chosen\\portrait.jpeg'),
            readDesktopPath,
            readDesktopHeader: async () => new Uint8Array([255, 216, 255]),
            chooseLegacyFile: vi.fn(),
            nativeImport,
            legacyImport,
        })

        expect(nativeImport).toHaveBeenCalledWith({
            source: { type: 'desktopPath', path: 'C:\\chosen\\portrait.jpeg' },
            displayName: 'portrait.jpeg',
            destination: { kind: 'current-character-image', characterId: 'character-1' },
        })
        expect(readDesktopPath).not.toHaveBeenCalled()
        expect(legacyImport).not.toHaveBeenCalled()
        expect(result).toEqual({ kind: 'selected', logicalId: 'assets/hash.jpeg' })
    })

    it('retains the legacy byte path for desktop non-JPEG images', async () => {
        const bytes = Uint8Array.from([8, 9])
        const nativeImport = vi.fn()
        const legacyImport = vi.fn(async (input: Uint8Array) => `legacy-${input[0]}`)

        const result = await selectCharacterImageFile('character-1', {
            isTauriDesktop: true,
            chooseDesktopPath: vi.fn(async () => 'C:\\chosen\\portrait.png'),
            readDesktopPath: vi.fn(async () => bytes),
            readDesktopHeader: async () => new Uint8Array([255, 216, 255]),
            chooseLegacyFile: vi.fn(),
            nativeImport,
            legacyImport,
        })

        expect(nativeImport).not.toHaveBeenCalled()
        expect(legacyImport).toHaveBeenCalledWith(bytes, 'portrait.png')
        expect(result).toEqual({ kind: 'selected', logicalId: 'legacy-8', bytes, name: 'portrait.png' })
    })

    it('retains the legacy picker and byte path on Web even for JPEG', async () => {
        const bytes = Uint8Array.from([0xff, 0xd8, 0xff, 0xd9])
        const nativeImport = vi.fn()
        const legacyImport = vi.fn(async () => 'legacy-jpeg')

        const result = await selectCharacterImageFile('character-1', {
            isTauriDesktop: false,
            chooseDesktopPath: vi.fn(),
            readDesktopPath: vi.fn(),
            readDesktopHeader: async () => new Uint8Array([255, 216, 255]),
            chooseLegacyFile: vi.fn(async () => ({ name: 'portrait.jpg', data: bytes })),
            nativeImport,
            legacyImport,
        })

        expect(nativeImport).not.toHaveBeenCalled()
        expect(legacyImport).toHaveBeenCalledWith(bytes, 'portrait.jpg')
        expect(result).toEqual({ kind: 'selected', logicalId: 'legacy-jpeg', bytes, name: 'portrait.jpg' })
    })
})
