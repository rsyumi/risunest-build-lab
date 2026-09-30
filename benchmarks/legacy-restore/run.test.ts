import { beforeEach, describe, expect, it, vi } from 'vitest'
// Match the WebView codec path instead of Node's internal Buffer API.
vi.hoisted(() => { vi.stubGlobal('Buffer', undefined) })
import { gunzipSync } from 'fflate'
import { unpack } from 'msgpackr/index-no-eval'
import { fixtureCharacter, planFixture } from './fixture'
import { runLegacyRestoreMeasurement } from './run'

const native = vi.hoisted(() => ({ invoke: vi.fn(), remove: vi.fn(), bytes: new Uint8Array(3_000_000), offset: 0, length: 0 }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }))
vi.mock('@tauri-apps/api/path', () => ({ join: async (...parts: string[]) => parts.join('/') }))
vi.mock('../../src/ts/storage/nativePaths', () => ({ nativeDataPath: async () => '/synthetic' }))
vi.mock('@tauri-apps/plugin-fs', () => ({
    SeekMode: { Start: 0 }, mkdir: vi.fn(), remove: native.remove,
    open: async () => ({
        write: async (bytes: Uint8Array) => {
            // Short writes exercise the source writer's complete-write loop.
            const count = Math.min(bytes.length, 65_536)
            native.bytes.set(bytes.subarray(0, count), native.offset)
            native.offset += count
            native.length = Math.max(native.length, native.offset)
            return count
        },
        seek: async (offset: number) => { native.offset = offset }, close: async () => {},
    }),
}))
vi.mock('./fixture', async importOriginal => {
    const actual = await importOriginal<typeof import('./fixture')>()
    return { ...actual, planFixture: () => actual.planFixture(2_500_000) }
})

beforeEach(() => {
    native.invoke.mockReset()
    native.remove.mockReset()
    native.bytes.fill(0)
    native.offset = native.length = 0
})
function installNative(corrupt = false) {
    const plan = planFixture(2_500_000)
    let finalized = false
    native.invoke.mockImplementation(async (command: string, args: any) => {
        if (command === 'pds_open') return { revision: 7 }
        if (command === 'native_file_job_start') return { jobId: 'synthetic-job' }
        if (command === 'native_file_job_finalize') { finalized = true; return true }
        if (command === 'native_file_job_status') return finalized
            ? { state: 'succeeded', phase: 'complete', result: { characterCount: plan.characterCount, sourceBytes: native.length } }
            : { state: 'running', phase: 'awaiting-activation' }
        if (command === 'pds_read_conversation') {
            const index = Number(args.characterId.slice('synthetic-'.length))
            const value = fixtureCharacter(plan, index).chats[0]
            if (corrupt) value.message[0].data = 'synthetic corruption'
            return { value }
        }
        if (command === 'native_file_job_forget') return true
        throw new Error(`Unexpected command ${command}`)
    })
    return plan
}

describe('isolated legacy restore measurement', () => {
    it.each(['raw', 'gzip'] as const)('writes upstream %s bytes and validates bounded native readback', async encoding => {
        const plan = installNative()
        const report = vi.fn(async () => {})
        const result = await runLegacyRestoreMeasurement({ megabytes: 100, encoding,
            assertIsolatedHarness: async () => {}, report,
            sampleMemory: async () => ({ peakRssBytes: 6_000_000, source: 'synthetic-test' }),
        })
        const frame = native.bytes.subarray(0, native.length)
        const view = new DataView(frame.buffer)
        const nameBytes = view.getUint32(0, true)
        expect(new TextDecoder().decode(frame.subarray(4, 4 + nameBytes))).toBe('database.risudat')
        expect(view.getUint32(4 + nameBytes, true)).toBe(frame.length - 8 - nameBytes)
        const database = frame.subarray(8 + nameBytes)
        expect([...database.subarray(0, 11)]).toEqual([0, 82, 73, 83, 85, 83, 65, 86, 69, 0, encoding === 'raw' ? 7 : 8])
        const decoded = encoding === 'gzip' ? gunzipSync(database.subarray(11)) : database.subarray(11)
        expect(decoded.length).toBe(plan.decodedBytes)
        expect(unpack(decoded).characters).toHaveLength(plan.characterCount)
        expect(result).toMatchObject({ phase: 'verified', verifiedMessageCount: plan.messageCount,
            verifiedCharacterCount: plan.characterCount, aboveTwiceDecoded: true })
        expect(native.invoke.mock.calls.filter(([command]) => command === 'pds_read_conversation')).toHaveLength(plan.characterCount)
        expect(native.invoke).toHaveBeenCalledWith('native_file_job_finalize', { jobId: 'synthetic-job', expectedRevision: 7 })
        expect(native.remove).toHaveBeenCalledTimes(2)
    })
    it('gates native restore after preparation and readback after the terminal interval', async () => {
        installNative()
        let prepared: Record<string, unknown> | undefined
        let terminal: Record<string, unknown> | undefined
        let releaseRestore!: () => void
        let releaseReadback!: () => void
        const restoreGate = new Promise<void>(resolve => { releaseRestore = resolve })
        const readbackGate = new Promise<void>(resolve => { releaseReadback = resolve })
        const measurement = runLegacyRestoreMeasurement({ megabytes: 100, encoding: 'raw',
            assertIsolatedHarness: async () => {}, report: async () => {},
            beforeRestore: async event => { prepared = event; await restoreGate },
            afterRestore: async event => { terminal = event; await readbackGate },
        })
        await vi.waitFor(() => expect(prepared?.phase).toBe('prepared'), { timeout: 10_000 })
        expect(native.length).toBeGreaterThan(2_500_000)
        expect(native.invoke.mock.calls.some(([command]) => command === 'native_file_job_start')).toBe(false)
        releaseRestore()
        await vi.waitFor(() => expect(terminal?.phase).toBe('restore-terminal'), { timeout: 10_000 })
        expect(terminal?.outcome).toBe('succeeded')
        expect(native.invoke.mock.calls.some(([command]) => command === 'pds_read_conversation')).toBe(false)
        expect(native.remove).not.toHaveBeenCalled()
        releaseReadback()
        expect(await measurement).toMatchObject({ phase: 'verified' })
        expect(native.invoke.mock.calls.some(([command]) => command === 'pds_read_conversation')).toBe(true)
        expect(native.remove).toHaveBeenCalledTimes(2)
    })
    it('rejects incorrect readback without reporting verification', async () => {
        installNative(true)
        const events: Record<string, unknown>[] = []
        await expect(runLegacyRestoreMeasurement({ megabytes: 100, encoding: 'raw',
            assertIsolatedHarness: async () => {}, report: async event => { events.push(event) },
        })).rejects.toThrow('message hash mismatch')
        expect(events.some(event => event.phase === 'verified')).toBe(false)
        expect(native.remove).not.toHaveBeenCalled()
    })
    it('requires isolation before writing or invoking native commands', async () => {
        await expect(runLegacyRestoreMeasurement({ megabytes: 100, encoding: 'raw',
            assertIsolatedHarness: async () => { throw new Error('isolation required') }, report: async () => {},
        })).rejects.toThrow('isolation required')
        expect(native.length).toBe(0)
        expect(native.invoke).not.toHaveBeenCalled()
    })
})
