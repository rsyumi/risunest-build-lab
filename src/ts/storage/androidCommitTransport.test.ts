import { describe, expect, it, vi } from 'vitest'
import { sendAndroidCommit, ANDROID_COMMIT_CHUNK_BYTES } from './androidCommitTransport'
import { prepareCasObject } from './nativeAssetRepository'
import {
    ANDROID_BINARY_CHUNK_BYTES,
    type AndroidBinaryCommitBridge,
} from './androidBinaryCommitBridge'

function harness(options: { fail?: string; badAck?: boolean } = {}) {
    const received: string[] = []
    const invoke = vi.fn(async (command: string, args: any) => {
        if (command === options.fail) throw new Error('synthetic failure')
        if (command === 'pds_commit_android_open') return { capacity: ANDROID_COMMIT_CHUNK_BYTES }
        if (command === 'pds_commit_android_chunk') {
            received.push(args.chunk)
            return (
                args.offset + new TextEncoder().encode(args.chunk).length + (options.badAck ? 1 : 0)
            )
        }
        return { revision: 3 }
    })
    return { invoke, received }
}

describe('Android bounded commit transport', () => {
    it('keeps concurrent database, device and asset writes on their own transfer until cleanup', async () => {
        vi.useFakeTimers()
        try {
            let active: string | undefined
            let assetId: string | undefined
            const received = new Map<string, Uint8Array[]>()
            const saved: Array<{ command: string; bytes: Uint8Array }> = []
            const bridge: AndroidBinaryCommitBridge = {
                onmessage: null,
                postMessage(packet) {
                    const bytes = new Uint8Array(packet)
                    const id = new TextDecoder().decode(bytes.subarray(0, 36))
                    if (id !== active && id !== assetId) throw new Error('Wrong native transfer')
                    received.get(id)!.push(bytes.slice(40))
                    queueMicrotask(() => bridge.onmessage?.({ data: JSON.stringify({
                        id, offset: new DataView(packet).getUint32(36, true) + bytes.length - 40,
                    }) }))
                },
            }
            vi.stubGlobal('window', { RisuNestCommit: bridge })
            const invoke = async (command: string, args: any) => {
                if (command === 'asset_cas_job_upload_open') {
                    assetId = args.uploadId
                    received.set(assetId!, [])
                    return { capacity: ANDROID_BINARY_CHUNK_BYTES }
                }
                if (command === 'asset_cas_job_upload_finish') {
                    saved.push({ command, bytes: Buffer.concat(received.get(assetId!)!) })
                    return { contentHash: '22'.repeat(32), physicalKey: `assets/objects/22/${'22'.repeat(31)}`,
                        byteSize: saved.at(-1)!.bytes.length, deduplicated: false }
                }
                if (command === 'asset_cas_job_upload_cancel') { assetId = undefined; return }
                if (command === 'pds_commit_android_open') {
                    if (active) throw new Error('Android persistence transfer already active')
                    active = args.id
                    received.set(args.id, [])
                    return { capacity: ANDROID_BINARY_CHUNK_BYTES }
                }
                if (command.endsWith('_finish')) {
                    if (args.id !== active) throw new Error('Wrong native finish')
                    saved.push({ command, bytes: Buffer.concat(received.get(args.id)!) })
                    return { revision: saved.length }
                }
                if (command === 'pds_commit_android_cancel' && args.id === active) active = undefined
            }
            const database = new TextEncoder().encode('database'.repeat(40_000))
            const device = new TextEncoder().encode('device'.repeat(24_000))
            const asset = Uint8Array.from({ length: 1024 * 1024 + 3 }, (_, i) => i % 256)
            const outcomes = Promise.allSettled([
                prepareCasObject('synthetic-session', asset, 'direct-object', invoke),
                sendAndroidCommit(database, invoke as any, bridge),
                sendAndroidCommit(device, invoke as any, bridge, 'pds_plugin_write_android_finish'),
            ])
            await vi.runAllTimersAsync()
            expect((await outcomes).map(result => result.status)).toEqual(['fulfilled', 'fulfilled', 'fulfilled'])
            expect(saved.map(item => item.command)).toEqual(['pds_commit_android_finish', 'pds_plugin_write_android_finish', 'asset_cas_job_upload_finish'])
            expect(Buffer.from(saved[0].bytes).equals(database)).toBe(true)
            expect(Buffer.from(saved[1].bytes).equals(device)).toBe(true)
            expect(Buffer.from(saved[2].bytes).equals(asset)).toBe(true)
            expect(active).toBeUndefined()
            expect(bridge.onmessage).toBeNull()
        } finally {
            vi.unstubAllGlobals()
            vi.useRealTimers()
        }
    })

    it('waits only for pre-transfer finishing-busy responses', async () => {
        vi.useFakeTimers()
        try {
            const h = harness()
            let opens = 0
            const invoke = vi.fn(async (command: string, args: any) => {
                if (command === 'pds_commit_android_open' && opens++ < 2) throw { code: 'commit-busy' }
                return h.invoke(command, args)
            })
            const result = sendAndroidCommit(new TextEncoder().encode('synthetic'), invoke as any, null)
            await vi.runAllTimersAsync()
            await expect(result).resolves.toEqual({ revision: 3 })
            expect(opens).toBe(3)
            expect(invoke.mock.calls.filter(([command]) => command === 'pds_commit_android_finish')).toHaveLength(1)
        } finally {
            vi.useRealTimers()
        }
    })
    function binaryHarness(
        options: {
            badAck?: boolean
            drop?: boolean
            reject?: boolean
            finishFails?: boolean
            busy?: number
        } = {},
    ) {
        const packets: Uint8Array[] = []
        const bridge: AndroidBinaryCommitBridge = {
            onmessage: null,
            postMessage(packet) {
                const bytes = new Uint8Array(packet)
                packets.push(bytes.slice(40))
                const id = new TextDecoder().decode(bytes.subarray(0, 36))
                const offset = new DataView(packet).getUint32(36, true) + bytes.length - 40
                if (!options.drop)
                    queueMicrotask(() =>
                        bridge.onmessage?.({
                            data: JSON.stringify({
                                id,
                                offset: offset + (options.badAck ? 1 : 0),
                                error: packets.length <= (options.busy ?? 0) ? 'busy' : options.reject ? 'invalid' : undefined,
                            }),
                        }),
                    )
            },
        }
        const invoke = vi.fn(async (command: string) => {
            if (command === 'pds_commit_android_open')
                return { capacity: ANDROID_BINARY_CHUNK_BYTES }
            if (command === 'pds_commit_android_finish' && options.finishFails)
                throw new Error('uncertain finish')
            return { revision: 4 }
        })
        return { bridge, invoke, packets }
    }
    it('uses exact bounded binary chunks, including split UTF-8, and releases the listener', async () => {
        const h = binaryHarness()
        const bytes = new TextEncoder().encode(
            'a'.repeat(ANDROID_BINARY_CHUNK_BYTES - 1) + '🐿️합성'.repeat(20),
        )
        expect(await sendAndroidCommit(bytes, h.invoke as any, h.bridge)).toEqual({
            revision: 4,
        })
        expect(Buffer.concat(h.packets).equals(bytes)).toBe(true)
        expect(h.packets.every((p) => p.length <= ANDROID_BINARY_CHUNK_BYTES)).toBe(true)
        expect(h.invoke).toHaveBeenCalledWith(
            'pds_commit_android_open',
            expect.objectContaining({ binary: true }),
        )
        expect(h.invoke.mock.calls.map(([name]) => name)).toEqual([
            'pds_commit_android_open',
            'pds_commit_android_finish',
            'pds_commit_android_cancel',
        ])
        expect(h.bridge.onmessage).toBeNull()
    })
    it('retries an explicit pre-admission busy reply without switching transport', async () => {
        vi.useFakeTimers()
        try {
            const h = binaryHarness({ busy: 1 })
            const bytes = new TextEncoder().encode('busy-byte-channel')
            const result = sendAndroidCommit(bytes, h.invoke as any, h.bridge)
            await vi.runAllTimersAsync()
            await expect(result).resolves.toEqual({ revision: 4 })
            expect(Buffer.concat(h.packets.slice(1)).equals(bytes)).toBe(true)
            expect(h.bridge.onmessage).toBeNull()
        } finally {
            vi.useRealTimers()
        }
    })
    it.each([{ badAck: true }, { reject: true }, { finishFails: true }])(
        'does not replay failed binary transfer %j',
        async (options) => {
            const h = binaryHarness(options)
            await expect(
                sendAndroidCommit(new TextEncoder().encode('{}'), h.invoke as any, h.bridge),
            ).rejects.toThrow()
            expect(h.bridge.onmessage).toBeNull()
            expect(h.invoke.mock.calls.at(-1)?.[0]).toBe('pds_commit_android_cancel')
            expect(
                h.invoke.mock.calls.some(([name]) =>
                    ['pds_commit', 'pds_commit_android_chunk'].includes(name),
                ),
            ).toBe(false)
            expect(
                h.invoke.mock.calls.filter(([name]) => name === 'pds_commit_android_finish'),
            ).toHaveLength(options.finishFails ? 1 : 0)
        },
    )
    it('times out a missing ACK, cancels, and does not retry through strings', async () => {
        vi.useFakeTimers()
        try {
            const h = binaryHarness({ drop: true })
            const result = sendAndroidCommit(
                new TextEncoder().encode('{}'),
                h.invoke as any,
                h.bridge,
            )
            const rejected = expect(result).rejects.toThrow('timed out')
            await vi.advanceTimersByTimeAsync(10001)
            await rejected
            expect(h.bridge.onmessage).toBeNull()
            expect(h.invoke.mock.calls.map(([name]) => name)).toEqual([
                'pds_commit_android_open',
                'pds_commit_android_cancel',
            ])
        } finally {
            vi.useRealTimers()
        }
    })
    it('keeps the string route when no native binary listener is available', async () => {
        const h = harness()
        await sendAndroidCommit(new TextEncoder().encode('{}'), h.invoke, null)
        expect(h.invoke).toHaveBeenCalledWith(
            'pds_commit_android_open',
            expect.objectContaining({ binary: false }),
        )
        expect(h.received).toEqual(['{}'])
    })
    it('preserves exact UTF-8 across every possible multibyte boundary', async () => {
        for (const char of ['é', '합', '🐿', '\\"\n', '\ufeff']) {
            for (let shift = 0; shift <= 4; shift++) {
                const text = 'a'.repeat(ANDROID_COMMIT_CHUNK_BYTES - shift) + char.repeat(5)
                const h = harness()
                expect(await sendAndroidCommit(new TextEncoder().encode(text), h.invoke)).toEqual({
                    revision: 3,
                })
                expect(h.received.join('')).toBe(text)
                expect(
                    h.received.every(
                        (chunk) =>
                            new TextEncoder().encode(chunk).length <= ANDROID_COMMIT_CHUNK_BYTES,
                    ),
                ).toBe(true)
                expect(h.invoke.mock.calls.map(([command]) => command).slice(-2)).toEqual([
                    'pds_commit_android_finish',
                    'pds_commit_android_cancel',
                ])
            }
        }
    })
    it.each(['pds_commit_android_open', 'pds_commit_android_chunk', 'pds_commit_android_finish'])(
        'cancels %s failure without replaying an uncertain save',
        async (fail) => {
            const h = harness({ fail })
            await expect(
                sendAndroidCommit(new TextEncoder().encode('{}'), h.invoke),
            ).rejects.toThrow('synthetic failure')
            expect(h.invoke).toHaveBeenLastCalledWith('pds_commit_android_cancel', {
                id: h.invoke.mock.calls[0][1].id,
            })
            expect(
                h.invoke.mock.calls.some(
                    ([cmd]) => cmd === 'pds_commit' || cmd === 'pds_commit_raw',
                ),
            ).toBe(false)
        },
    )
    it('rejects a wrong acknowledgement before commit', async () => {
        const h = harness({ badAck: true })
        await expect(sendAndroidCommit(new TextEncoder().encode('{}'), h.invoke)).rejects.toThrow(
            'acknowledgement',
        )
        expect(h.invoke.mock.calls.some(([cmd]) => cmd === 'pds_commit_android_finish')).toBe(false)
    })
    it('rejects an invalid capacity and malformed UTF-8 without a save', async () => {
        const h = harness()
        h.invoke.mockResolvedValueOnce({ capacity: 0 } as any)
        await expect(sendAndroidCommit(new Uint8Array([123, 125]), h.invoke)).rejects.toThrow(
            'capacity',
        )
        await expect(sendAndroidCommit(new Uint8Array([0xff]), h.invoke)).rejects.toThrow()
        expect(h.invoke.mock.calls.some(([cmd]) => cmd === 'pds_commit_android_finish')).toBe(false)
    })
})
