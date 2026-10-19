import { describe, expect, it } from 'vitest'
import { language } from 'src/lang'
import { imageFixture } from '../../../../tests/support/imageFixtures'
import { imageAliases, imageFormatForName, requireImage, selectedImageFormat, stripImageExtension } from '../imageFormats'

describe('shared image content contract', () => {
    it.each(['png', 'jpg', 'gif', 'webp', 'avif'] as const)('identifies %s independently of the name without changing bytes', ext => {
        const bytes = imageFixture(ext)
        const original = bytes.slice()
        expect(requireImage(bytes)).toMatchObject({ ext, mime: ext === 'jpg' ? 'image/jpeg' : `image/${ext}` })
        for (const name of ['photo.PNG', 'provider-item', 'photo.mp3']) expect(selectedImageFormat(bytes, name)?.ext).toBe(ext)
        expect(bytes).toEqual(original)
    })

    it.each(['png', 'gif', 'webp', 'avif'] as const)('keeps animated %s in the original family', ext => {
        expect(requireImage(imageFixture(ext, true)).ext).toBe(ext)
    })

    it.each(imageAliases)('accepts mixed-case alias %s and strips only the last suffix', alias => {
        const name = `Smile.PNG.${alias.toUpperCase()}`
        expect(imageFormatForName(name)).toBeDefined()
        expect(stripImageExtension(name)).toBe('Smile.PNG')
    })

    it('finds AVIF in compatible brands after another ISO box', () => {
        const bytes = new Uint8Array([0,0,0,8, ...new TextEncoder().encode('free'), 0,0,0,24, ...new TextEncoder().encode('ftypmif1'), 0,0,0,0, ...new TextEncoder().encode('miafavif')])
        expect(requireImage(bytes).ext).toBe('avif')
        new DataView(bytes.buffer).setUint32(8, 100)
        expect(() => requireImage(bytes)).toThrow(language.risuNest.imageFormats.damaged)
    })

    it.each(['png', 'jpg', 'gif', 'webp', 'avif'] as const)('rejects a damaged %s header before storage', ext => {
        const length = { png: 8, jpg: 3, gif: 6, webp: 12, avif: 12 }[ext]
        expect(() => requireImage(imageFixture(ext).slice(0, length))).toThrow(language.risuNest.imageFormats.damaged)
    })

    it('rejects wrong dimensions, missing IHDR and oversized RIFF headers', () => {
        const png = imageFixture('png')
        new DataView(png.buffer).setUint32(16, 0)
        expect(() => requireImage(png)).toThrow(language.risuNest.imageFormats.damaged)
        const invalidDepth = imageFixture('png')
        invalidDepth[24] = 1
        expect(() => requireImage(invalidDepth)).toThrow(language.risuNest.imageFormats.damaged)
        png[12] = 65
        expect(() => requireImage(png)).toThrow(language.risuNest.imageFormats.damaged)
        const webp = imageFixture('webp')
        new DataView(webp.buffer).setUint32(4, webp.length + 1, true)
        expect(() => requireImage(webp)).toThrow(language.risuNest.imageFormats.damaged)
        new DataView(webp.buffer).setUint32(4, 12, true)
        expect(() => requireImage(webp)).toThrow(language.risuNest.imageFormats.damaged)
    })

    it.each(['<svg/>', 'BMheader', 'II*\0tiff', '\0\0\0\x18ftypheic', 'not an image'])('refuses unsupported content %j only for image use', value => {
        const bytes = new TextEncoder().encode(value)
        expect(() => requireImage(bytes)).toThrow(language.risuNest.imageFormats.unsupported)
        expect(() => selectedImageFormat(bytes, 'wrong.PNG')).toThrow(language.risuNest.imageFormats.unsupported)
        expect(selectedImageFormat(bytes, 'generic.svg')).toBeNull()
    })
})
