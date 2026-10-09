import { describe, expect, it, vi } from 'vitest'
import { createNativeImageGeometryStore, type ImageGeometry } from './imageGeometry'

const geometry = (index: number): ImageGeometry => ({ contentHash: index.toString(16).padStart(64, '0'), width: 800, height: 600 })

describe('native image geometry batches', () => {
    it('coalesces duplicate reads and bounds IPC for a large simultaneous lookup', async () => {
        const command = vi.fn(async (_name, args) => (args.hashes as string[]).map(contentHash => ({ ...geometry(1), contentHash })))
        const store = createNativeImageGeometryStore(command as never)
        const values = await Promise.all(Array.from({ length: 201 }, (_, index) => store.read([geometry(index % 200).contentHash])))
        expect(command).toHaveBeenCalledTimes(4)
        expect(command.mock.calls.every(([, args]) => args.hashes.length <= 64)).toBe(true)
        expect(values[0]).toEqual(values[200])
        expect(values[5]).toEqual([geometry(5)])
    })

    it('coalesces writes, reports failed disk writes and permits retry', async () => {
        const command = vi.fn().mockRejectedValueOnce(new Error('disk full')).mockResolvedValue(undefined)
        const store = createNativeImageGeometryStore(command)
        const results = await Promise.allSettled([store.write([geometry(1)]), store.write([geometry(1)])])
        expect(results.map(result => result.status)).toEqual(['rejected', 'rejected'])
        expect(command).toHaveBeenCalledTimes(1)
        await store.write(Array.from({ length: 130 }, (_, index) => geometry(index)))
        expect(command.mock.calls.slice(1).map(([, args]) => args.values.length)).toEqual([64, 64, 2])
    })

    it('rejects conflicting sizes without acknowledging them as stored', async () => {
        const command = vi.fn().mockResolvedValue(undefined)
        const store = createNativeImageGeometryStore(command)
        const first = store.write([geometry(1)])
        await expect(store.write([{ ...geometry(1), width: 900 }])).rejects.toThrow('Conflicting')
        await first
        expect(command.mock.calls[0][1].values).toEqual([geometry(1)])
    })

    it('cancels queued writes and stale reads across storage activation', async () => {
        let epoch = 0
        const command = vi.fn().mockImplementation(async () => { epoch++; return [geometry(1)] })
        const store = createNativeImageGeometryStore(command, () => epoch)
        const write = store.write([geometry(1)])
        epoch++
        await expect(write).rejects.toMatchObject({ name: 'AbortError' })
        expect(command).not.toHaveBeenCalled()
        await expect(store.read([geometry(1).contentHash])).rejects.toMatchObject({ name: 'AbortError' })
    })

    it('rejects invalid and mismatched native responses', async () => {
        const store = createNativeImageGeometryStore(vi.fn().mockResolvedValue([geometry(2)]))
        await expect(store.read([geometry(1).contentHash])).rejects.toThrow('Mismatched')
        await expect(store.write([{ ...geometry(1), width: 0 }])).rejects.toThrow('Invalid')
    })
})
