import { Sha256 } from '@aws-crypto/sha256-js'
import { SafeLocalPluginStorage, SafeLocalStorage } from '../../src/ts/plugins/pluginSafeClass'
import { pluginDeviceValueSize } from '../../src/ts/plugins/pluginDeviceTransfer'

type Kind = 'string' | 'json'
type Phase = 'idle' | 'constructing' | 'writing' | 'reading' | 'backup' | 'restore' | 'clearing' | 'verified' | 'failed'
const owner = 'synthetic-large-plugin-values'
const key = 'value'
const pattern = '가나다😀'
const encoder = new TextEncoder()
const strings = new SafeLocalStorage(owner)
const json = new SafeLocalPluginStorage(owner)

async function digest(value: string): Promise<string> {
    const hash = new Sha256()
    const chunkUnits = 1024 * 1024
    const buffer = new Uint8Array(3 * chunkUnits)
    for (let offset = 0; offset < value.length;) {
        let end = Math.min(value.length, offset + chunkUnits)
        if (end < value.length && value.charCodeAt(end - 1) >= 0xd800 && value.charCodeAt(end - 1) <= 0xdbff) end--
        const chunk = value.slice(offset, end)
        const encoded = encoder.encodeInto(chunk, buffer)
        if (encoded.read !== chunk.length) throw new Error('Synthetic hash buffer is too small')
        hash.update(buffer.subarray(0, encoded.written))
        offset = end
        await new Promise(resolve => setTimeout(resolve, 0))
    }
    return Array.from(await hash.digest(), byte => byte.toString(16).padStart(2, '0')).join('')
}

export function pluginValueMeasurement(dependencies: {
    ready(): Promise<void>
    backup(): Promise<string>
    restore(path: string): Promise<unknown>
}) {
    let state: Record<string, unknown> & { phase: Phase } = { phase: 'idle' }
    let kind: Kind = 'string'
    let expected = ''
    let expectedLength = 0
    let archive = ''
    let busy = false
    async function operation(action: () => Promise<void>) {
        if (busy) throw new Error('Synthetic measurement is busy')
        busy = true
        try { await dependencies.ready(); await action() }
        catch (error) { state = { ...state, phase: 'failed', failure: error instanceof Error ? error.message : String(error), stack: error instanceof Error ? error.stack : undefined } }
        finally { busy = false }
    }
    async function verify() {
        state = { ...state, phase: 'reading' }
        const value = kind === 'string' ? await strings.getItem(key) : (await json.getItem<{ payload: string }>(key))?.payload
        if (typeof value !== 'string' || value.length !== expectedLength || await digest(value) !== expected)
            throw new Error('Synthetic plugin value differs')
        state = { ...state, phase: 'verified', hash: expected, codeUnits: expectedLength }
    }
    return {
        ready: dependencies.ready,
        state: () => ({ ...state, busy }),
        async write(mebibytes: number, selected: Kind) {
            await operation(async () => {
                if (![64, 256, 512].includes(mebibytes) || !['string', 'json'].includes(selected)) throw new Error('Unknown synthetic value case')
                kind = selected
                const encodedBytes = mebibytes * 1024 * 1024
                state = { phase: 'constructing', mebibytes, kind, encodedBytes }
                const overhead = kind === 'string' ? 2 : pluginDeviceValueSize(JSON.stringify({ payload: '' }))
                const bytes = encodedBytes - overhead
                const patternBytes = encoder.encode(pattern).length
                const repeats = Math.floor(bytes / patternBytes)
                let value: string
                if (kind === 'json') {
                    // Join shared small chunks into a flat string without another full JSON source string.
                    const chunkRepeats = 8192
                    const chunks = Array<string>(Math.floor(repeats / chunkRepeats)).fill(pattern.repeat(chunkRepeats))
                    chunks.push(pattern.repeat(repeats % chunkRepeats), 'x'.repeat(bytes % patternBytes))
                    value = chunks.join('')
                } else value = pattern.repeat(repeats) + 'x'.repeat(bytes % patternBytes)
                expectedLength = value.length
                expected = await digest(value)
                state = { ...state, phase: 'writing', hash: expected, codeUnits: expectedLength }
                if (kind === 'string') await strings.setItem(key, value)
                else await json.setItem(key, { payload: value })
                value = ''
                state = { ...state, phase: 'verified' }
            })
        },
        async read() { await operation(verify) },
        async backup() { await operation(async () => { state = { ...state, phase: 'backup' }; archive = await dependencies.backup(); state = { ...state, phase: 'verified', archive } }) },
        async restore() { await operation(async () => {
            state = { ...state, phase: 'clearing' }
            if (kind === 'string') await strings.removeItem(key); else await json.removeItem(key)
            state = { ...state, phase: 'restore' }
            await dependencies.restore(archive)
            await verify()
        }) },
        async expect(descriptor: { kind: Kind; hash: string; codeUnits: number }) {
            if (!['string', 'json'].includes(descriptor.kind) || !/^[a-f0-9]{64}$/.test(descriptor.hash) || !Number.isSafeInteger(descriptor.codeUnits)) throw new Error('Invalid synthetic descriptor')
            kind = descriptor.kind; expected = descriptor.hash; expectedLength = descriptor.codeUnits
            await operation(verify)
        },
    }
}
