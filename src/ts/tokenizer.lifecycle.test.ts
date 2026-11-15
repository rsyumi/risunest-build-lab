import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import encodeCases from '../../tests/fixtures/rust-core/tokenizer/encode.cases.json'
import encodeExpected from '../../tests/fixtures/rust-core/tokenizer/encode.expected.json'

vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: () => () => {} }
})
vi.mock('@mlc-ai/web-tokenizers', async (original) => {
    const actual = await original<typeof import('@mlc-ai/web-tokenizers')>()
    return { ...actual, ...(globalThis as { tokenizers?: object }).tokenizers }
})
import { encodeWithTokenizer } from './tokenizer'

function reference(id: string) {
    const entry = encodeCases.cases.find(row => row.id === id)
    const observed = encodeExpected.cases.find(row => row.id === id)?.result
    if (!entry || typeof entry.input !== 'string' || !observed || !('ids' in observed) || !Array.isArray(observed.ids)) {
        throw new Error(`Missing successful tokenizer reference: ${id}`)
    }
    return { text: entry.input, ids: observed.ids }
}
const mistral = reference('named-mistral')
const llama = reference('named-llama')
const claude = reference('named-claude')
const assets = new Set(['/token/mistral/tokenizer.model', '/token/llama/llama.model', '/token/claude/claude.json'])
async function response(input: RequestInfo | URL) {
    const url = String(input)
    if (!assets.has(url)) throw new Error(`Unexpected tokenizer asset: ${url}`)
    return new Response(await readFile(resolve('public', url.slice(1))))
}
function gate() {
    let resolve!: () => void
    let reject!: (error: Error) => void
    const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no })
    return { promise, resolve, reject }
}

beforeAll(async () => {
    const { Tokenizer } = await import('@mlc-ai/web-tokenizers')
    const node = Object.getOwnPropertyDescriptor(process.versions, 'node')!
    Object.defineProperty(process.versions, 'node', { ...node, value: undefined })
    try {
        const bytes = await readFile(resolve('public/token/mistral/tokenizer.model'))
        const tokenizer = await Tokenizer.fromSentencePiece(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer)
        tokenizer.dispose()
    } finally { Object.defineProperty(process.versions, 'node', node) }
})
beforeEach(() => { vi.spyOn(globalThis, 'fetch').mockImplementation(response) })
afterEach(() => vi.restoreAllMocks())

describe('shared tokenizer replacement lifetime', () => {
    it('returns exact IDs when same-family loads complete in reverse order', async () => {
        expect([...await encodeWithTokenizer(mistral.text, 'mistral')]).toEqual(mistral.ids)
        const started = gate()
        const release = gate()
        let delayed = false
        vi.mocked(fetch).mockImplementation(async (input) => {
            if (String(input) === '/token/claude/claude.json' && !delayed) {
                delayed = true
                started.resolve()
                await release.promise
            }
            return await response(input)
        })
        const slow = encodeWithTokenizer(claude.text, 'claude')
        try {
            await started.promise
            expect([...await encodeWithTokenizer(claude.text, 'claude')]).toEqual(claude.ids)
        } finally { release.resolve(); await slow.catch(() => {}) }
        expect([...await slow]).toEqual(claude.ids)
        expect([...await encodeWithTokenizer(claude.text, 'claude')]).toEqual(claude.ids)
    })

    it('returns each family IDs when different-family loads complete in reverse order', async () => {
        expect([...await encodeWithTokenizer(mistral.text, 'mistral')]).toEqual(mistral.ids)
        const started = gate()
        const release = gate()
        vi.mocked(fetch).mockImplementation(async (input) => {
            if (String(input) === '/token/claude/claude.json') {
                started.resolve()
                await release.promise
            }
            return await response(input)
        })
        const slow = encodeWithTokenizer(claude.text, 'claude')
        try {
            await started.promise
            expect([...await encodeWithTokenizer(llama.text, 'llama')]).toEqual(llama.ids)
        } finally { release.resolve(); await slow.catch(() => {}) }
        expect([...await slow]).toEqual(claude.ids)
        expect([...await encodeWithTokenizer(claude.text, 'claude')]).toEqual(claude.ids)
        expect([...await encodeWithTokenizer(llama.text, 'llama')]).toEqual(llama.ids)
    })

    it('keeps the current family usable during and after a failed replacement, then retries', async () => {
        expect([...await encodeWithTokenizer(claude.text, 'claude')]).toEqual(claude.ids)
        const started = gate()
        const release = gate()
        let delayed = false
        vi.mocked(fetch).mockImplementation(async (input) => {
            if (String(input) === '/token/mistral/tokenizer.model' && !delayed) {
                delayed = true
                started.resolve()
                await release.promise
            }
            return await response(input)
        })
        const failed = expect(encodeWithTokenizer(mistral.text, 'mistral')).rejects.toThrow('synthetic tokenizer load failure')
        try {
            await started.promise
            expect([...await encodeWithTokenizer(claude.text, 'claude')]).toEqual(claude.ids)
        } finally {
            release.reject(new Error('synthetic tokenizer load failure'))
            await failed.catch(() => {})
        }
        await failed
        expect([...await encodeWithTokenizer(claude.text, 'claude')]).toEqual(claude.ids)
        expect([...await encodeWithTokenizer(mistral.text, 'mistral')]).toEqual(mistral.ids)
        expect([...await encodeWithTokenizer(mistral.text, 'mistral')]).toEqual(mistral.ids)
    })
})