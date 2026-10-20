// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createHash, webcrypto } from 'node:crypto'
import { imageFixture } from '../../../tests/support/imageFixtures'
import { language } from 'src/lang'
import type { CharacterDetail } from './persistentDataStore'

const native = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', async importOriginal => ({
    ...await importOriginal<typeof import('@tauri-apps/api/core')>(), invoke: native.invoke,
}))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'windows' }))

import { prepareCharacterPortraitRendition } from './characterPortraitRendition'

const character = { image: 'assets/legacy.png' } as CharacterDetail
const staging = new Map<string, Uint8Array>()
const leases = new Set<string>()
let source: Uint8Array
let png: Uint8Array
let sourceHash: string
let missing: boolean
let failure: string
let controller: AbortController

beforeEach(() => {
    source = imageFixture('avif')
    png = imageFixture('png')
    sourceHash = createHash('sha256').update(source).digest('hex')
    missing = false
    failure = ''
    controller = new AbortController()
    staging.clear()
    leases.clear()
    vi.stubGlobal('crypto', webcrypto)
    vi.stubGlobal('createImageBitmap', vi.fn(async () => {
        if (failure === 'decode') throw new Error('decoder refused')
        return { width: 4, height: 3, close() {} }
    }))
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ drawImage() {} } as unknown as CanvasRenderingContext2D)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation(callback => callback(new Blob([new Uint8Array(png)], { type: 'image/png' })))
    native.invoke.mockReset().mockImplementation(async (command: string, args: Record<string, any> = {}) => {
        switch (command) {
            case 'pds_acquire_revision':
                if (args.revision !== 7) throw { code: 'revision-conflict', expected: args.revision, actual: 7 }
                leases.add('portrait-lease')
                return { lease: 'portrait-lease' }
            case 'pds_read_asset_alias':
                if (!leases.has(args.lease) || args.key !== character.image || args.kind !== 'asset') throw new Error('Invalid pinned read')
                return { revision: 7, value: { objectHash: sourceHash, size: source.length } }
            case 'pds_release_revision':
                leases.delete(args.lease)
                return
            case 'asset_cas_read_bytes':
                if (missing) throw 'cas-object-missing'
                if (args.contentHash !== sourceHash || args.expectedSize !== source.length) throw new Error('Invalid CAS read')
                return source.slice(args.start, args.endExclusive)
            case 'asset_remote_hydrate_object':
                missing = false
                return source.length
            case 'native_download_handoff_create':
                staging.set('/owned/rendition.bin', new Uint8Array())
                return '/owned/rendition.bin'
            case 'native_download_handoff_append': {
                const previous = staging.get(args.path)
                if (!previous || previous.length !== args.offset) throw new Error('Invalid append')
                const chunk = Buffer.from(args.chunk, 'base64')
                staging.set(args.path, new Uint8Array(Buffer.concat([previous, chunk])))
                if (failure === 'cancel') controller.abort()
                if (failure === 'append') throw new Error('disk full')
                return args.offset + chunk.length + (failure === 'ack' ? 1 : 0)
            }
            case 'native_download_handoff_cleanup':
                staging.delete(args.path)
                return
            default: throw new Error(`Unexpected IPC command: ${command}`)
        }
    })
})

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals() })

describe('portrait rendition preparation through native IPC', () => {
    it.each([false, true])('stages the exact PNG and releases its pinned source, remote hydration: %s', async remote => {
        missing = remote
        const expanded = new Uint8Array(70_000)
        expanded.set(source)
        source = expanded
        sourceHash = createHash('sha256').update(source).digest('hex')
        png = new Uint8Array(1024 * 1024 + 13)
        png.set(imageFixture('png'))
        const result = await prepareCharacterPortraitRendition(character, 7, 'png-card')
        expect(result?.descriptor.sourceHash).toBe(sourceHash)
        expect(staging.get(result!.descriptor.path)).toEqual(png)
        expect(leases.size).toBe(0)
        await result!.release()
        expect(staging.size).toBe(0)
    })

    it.each(['png', 'jpg', 'gif', 'webp'] as const)('leaves native %s portraits without a rendition', async ext => {
        source = imageFixture(ext)
        expect(await prepareCharacterPortraitRendition(character, 7, 'png-card')).toBeUndefined()
        expect(leases.size).toBe(0)
        expect(staging.size).toBe(0)
    })

    it.each([
        ['BMP', new Uint8Array([0x42, 0x4d, 0, 0, 0, 0])],
        ['HEIC', new Uint8Array([0, 0, 0, 24, 0x66, 0x74, 0x79, 0x70, 0x68, 0x65, 0x69, 0x63])],
    ])('offers a pinned legacy %s portrait to the WebView decoder', async (_format, bytes) => {
        source = bytes as Uint8Array
        sourceHash = createHash('sha256').update(source).digest('hex')
        const result = await prepareCharacterPortraitRendition(character, 7, 'png-card')
        expect(result?.descriptor.sourceHash).toBe(sourceHash)
        expect(staging.get(result!.descriptor.path)).toEqual(png)
        expect(leases.size).toBe(0)
        await result!.release()
        expect(staging.size).toBe(0)
    })

    it.each(['hash', 'stale', 'decode', 'append', 'ack', 'cancel'])('leaves no lease or staged file after %s failure', async mode => {
        failure = mode
        if (mode === 'hash') sourceHash = '0'.repeat(64)
        await expect(prepareCharacterPortraitRendition(character, mode === 'stale' ? 6 : 7, 'appended-charx-jpeg', controller.signal)).rejects.toBeDefined()
        expect(leases.size).toBe(0)
        expect(staging.size).toBe(0)
        if (mode === 'hash') expect(createImageBitmap).not.toHaveBeenCalled()
    })

    it('reports WebView decode failure with the export message', async () => {
        failure = 'decode'
        await expect(prepareCharacterPortraitRendition(character, 7, 'png-card')).rejects.toThrow(language.risuNest.imageFormats.portraitExportFailed)
    })
})
