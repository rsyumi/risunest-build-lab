import { describe, expect, it, vi } from 'vitest'
import { AndroidSafDestinationError, type AndroidSafDestinationRequest } from './androidSafBridge'
import { ANDROID_DOWNLOAD_PIECE_BYTES, downloadThroughAndroidSaf } from './androidSafDownload'
import { STAGED_REQUEST_BYTES } from './nativeCommitTransport'

const handoff = '/data/app/native-file-jobs/handoffs/risu-download-0f9d2c4a-1b2c-4d3e-8f4a-5b6c7d8e9f0a.bin'

function harness(copyResult: (request: AndroidSafDestinationRequest) => Promise<{ bytes: number }>) {
    const events: string[] = []
    const written: Uint8Array[] = []
    const invoke = vi.fn(async (command: string, args?: Record<string, unknown>): Promise<any> => {
        events.push(command)
        if (command === 'native_download_handoff_create') return handoff
        if (command === 'native_download_handoff_append') {
            const piece = Buffer.from(args!.chunk as string, 'base64')
            written.push(piece)
            return (args!.offset as number) + piece.length
        }
        return true
    })
    const copy = vi.fn(async (request: AndroidSafDestinationRequest) => {
        events.push('copy')
        const result = await copyResult(request)
        events.push('copy settled')
        return { warningCodes: [], ...result }
    })
    return { events, written, invoke, copy, dependencies: { invoke, copy } }
}

describe('downloadThroughAndroidSaf', () => {
    it('writes the bytes to the handoff in bounded pieces, copies it under the name and then removes it', async () => {
        const data = Uint8Array.from({ length: 2 * ANDROID_DOWNLOAD_PIECE_BYTES + 5 }, (_, index) => index % 251)
        const h = harness(async () => ({ bytes: data.length }))

        await expect(downloadThroughAndroidSaf('Alice_chat.json', data, h.dependencies)).resolves.toBe(true)

        const appends = h.invoke.mock.calls.filter(([command]) => command === 'native_download_handoff_append')
        expect(appends.map(([, args]) => args!.offset)).toEqual([0, ANDROID_DOWNLOAD_PIECE_BYTES, 2 * ANDROID_DOWNLOAD_PIECE_BYTES])
        expect(appends.every(([, args]) => args!.path === handoff)).toBe(true)
        for (const [command, args] of appends) {
            expect(command.length + JSON.stringify(args).length).toBeLessThanOrEqual(STAGED_REQUEST_BYTES - 2048)
        }
        expect(Buffer.concat(h.written).equals(Buffer.from(data))).toBe(true)
        expect(h.copy).toHaveBeenCalledWith({ sourcePath: handoff, suggestedName: 'Alice_chat.json' })
        expect(h.events.slice(-3)).toEqual(['copy', 'copy settled', 'native_download_handoff_cleanup'])
        expect(h.invoke).toHaveBeenLastCalledWith('native_download_handoff_cleanup', { path: handoff })
    })

    it('resolves false when the picker is cancelled and removes the handoff after the copy settles', async () => {
        const h = harness(async () => {
            throw Object.assign(new DOMException('cancelled', 'AbortError'), { requestId: 'request-1', warningCodes: [] })
        })

        await expect(downloadThroughAndroidSaf('chat.txt', Uint8Array.of(1, 2), h.dependencies)).resolves.toBe(false)

        expect(h.events).toEqual([
            'native_download_handoff_create',
            'native_download_handoff_append',
            'copy',
            'native_download_handoff_cleanup',
        ])
    })

    it('rethrows a failed copy after removing the handoff', async () => {
        const failure = new AndroidSafDestinationError('request-1', 'destination-busy', [], 'busy')
        const h = harness(async () => { throw failure })

        await expect(downloadThroughAndroidSaf('chat.txt', Uint8Array.of(1), h.dependencies)).rejects.toBe(failure)

        expect(h.events.at(-1)).toBe('native_download_handoff_cleanup')
    })

    it('stops at a failed append without opening the picker', async () => {
        const h = harness(async () => ({ bytes: 1 }))
        const failure = new Error('append failed')
        h.invoke.mockImplementation(async (command: string) => {
            h.events.push(command)
            if (command === 'native_download_handoff_create') return handoff
            if (command === 'native_download_handoff_append') throw failure
            return true
        })

        await expect(downloadThroughAndroidSaf('chat.txt', Uint8Array.of(1), h.dependencies)).rejects.toBe(failure)

        expect(h.copy).not.toHaveBeenCalled()
        expect(h.events.at(-1)).toBe('native_download_handoff_cleanup')
    })

    it('refuses a handoff or a copy whose length differs from the download', async () => {
        const short = harness(async () => ({ bytes: 3 }))
        short.invoke.mockImplementation(async (command: string) =>
            command === 'native_download_handoff_create' ? handoff : command === 'native_download_handoff_append' ? 2 : true)
        await expect(downloadThroughAndroidSaf('a.bin', Uint8Array.of(1, 2, 3), short.dependencies)).rejects.toThrow('length')
        expect(short.copy).not.toHaveBeenCalled()

        const copied = harness(async () => ({ bytes: 2 }))
        await expect(downloadThroughAndroidSaf('a.bin', Uint8Array.of(1, 2, 3), copied.dependencies)).rejects.toThrow('length')
        expect(copied.events.at(-1)).toBe('native_download_handoff_cleanup')
    })

    it('keeps the outcome when removing the handoff fails', async () => {
        const errors = vi.spyOn(console, 'error').mockImplementation(() => {})
        const h = harness(async () => ({ bytes: 1 }))
        const invoke = h.invoke.getMockImplementation()!
        h.invoke.mockImplementation(async (command, args) => {
            if (command === 'native_download_handoff_cleanup') throw new Error('cleanup failed')
            return invoke(command, args)
        })

        await expect(downloadThroughAndroidSaf('a.bin', Uint8Array.of(1), h.dependencies)).resolves.toBe(true)

        expect(errors).toHaveBeenCalledOnce()
        errors.mockRestore()
    })
})
