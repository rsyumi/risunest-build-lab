import { beforeEach, describe, expect, it, vi } from 'vitest'

type NativeRequest = { tokenizer_id: string; artifact_fingerprint: string; mode: 'count' | 'ids'; texts: string[] }

const harness = vi.hoisted(() => ({ invoke: vi.fn() }))

vi.mock('@tauri-apps/api/core', () => ({ invoke: harness.invoke }))

// A native count is the text's length; a JavaScript count is ten times that, so each count shows which side made it.
function nativeAnswer(_command: string, args: { request: NativeRequest }) {
    const { mode, artifact_fingerprint, texts } = args.request
    return mode === 'count'
        ? { mode, artifact_fingerprint, counts: texts.map(text => text.length) }
        : { mode, artifact_fingerprint, ids: texts.map(text => Array.from(text, () => 7)) }
}

const javaScript = vi.fn(async (text: string) => new Array<number>(text.length * 10).fill(0))

// The texts of each invoke, in order.
function sent() {
    return harness.invoke.mock.calls.map(([, args]) => (args as { request: NativeRequest }).request.texts)
}

// Fresh modules, so the session latch starts empty.
async function load() {
    vi.resetModules()
    return {
        ...await import('./nativeTokenizerCoalescer'),
        ...await import('./nativeTokenizerProduction'),
    }
}

describe('native tokenizer coalescer', () => {
    beforeEach(() => {
        harness.invoke.mockReset()
        harness.invoke.mockImplementation(nativeAnswer)
        javaScript.mockClear()
    })

    it('sends the texts requested in one turn as one invoke and answers each request in order', async () => {
        const { nativeTokenizerCounts, nativeTokenizerIds } = await load()

        const [first, second, ids] = await Promise.all([
            nativeTokenizerCounts('cl100k_base', ['one', 'three'], javaScript),
            nativeTokenizerCounts('cl100k_base', ['ab'], javaScript),
            nativeTokenizerIds('cl100k_base', ['xyz'], javaScript),
        ])

        expect(first).toEqual([3, 5])
        expect(second).toEqual([2])
        expect(ids).toEqual([[7, 7, 7]])
        expect(sent()).toEqual([['one', 'three', 'ab'], ['xyz']])
        expect(javaScript).not.toHaveBeenCalled()
    })

    it('splits a turn at 1,000 texts', async () => {
        const { nativeTokenizerCounts } = await load()
        const texts = Array.from({ length: 1_001 }, (_, index) => `text ${index}`)

        await expect(nativeTokenizerCounts('o200k_base', texts, javaScript)).resolves.toEqual(texts.map(text => text.length))

        expect(sent().map(batch => batch.length)).toEqual([1_000, 1])
    })

    it('splits a turn at 256 KB of UTF-8 rather than of string length', async () => {
        const { nativeTokenizerCounts } = await load()
        // 100,000 UTF-16 units each, but 300,000 bytes of UTF-8.
        const korean = ['가'.repeat(100_000), '나'.repeat(100_000)]
        const ascii = ['a'.repeat(100_000), 'b'.repeat(100_000), 'c'.repeat(100_000)]

        await nativeTokenizerCounts('cl100k_base', korean, javaScript)
        await nativeTokenizerCounts('cl100k_base', ascii, javaScript)

        expect(sent().map(batch => batch.length)).toEqual([1, 1, 2, 1])
        expect(javaScript).not.toHaveBeenCalled()
    })

    it('counts a text past the 1 MiB input limit in JavaScript without sending it', async () => {
        const { nativeTokenizerCounts } = await load()
        // 350,000 UTF-16 units, 1,050,000 bytes of UTF-8.
        const long = '가'.repeat(350_000)

        await expect(nativeTokenizerCounts('cl100k_base', ['short', long], javaScript)).resolves.toEqual([5, 3_500_000])

        expect(sent()).toEqual([['short']])
        expect(javaScript).toHaveBeenCalledExactlyOnceWith(long)
    })

    it('counts a lone surrogate in JavaScript for a model family and sends the rest in one invoke', async () => {
        const { nativeTokenizerCounts } = await load()

        await expect(nativeTokenizerCounts('claude', ['a', 'b', '\ud800', 'c'], javaScript)).resolves.toEqual([1, 1, 10, 1])

        expect(sent()).toEqual([['a', 'b', 'c']])
    })

    it('counts a refused text in JavaScript, sends the rest again, and keeps the family native', async () => {
        const { nativeTokenizerCounts, isNativeTokenizerUnavailable } = await load()
        harness.invoke.mockRejectedValueOnce({ code: 'disallowed_special_token', message: 'synthetic refusal', index: 1 })

        await expect(nativeTokenizerCounts('cl100k_base', ['a', '<|endoftext|>', 'cc'], javaScript)).resolves.toEqual([1, 130, 2])
        await expect(nativeTokenizerCounts('cl100k_base', ['next'], javaScript)).resolves.toEqual([4])

        expect(sent()).toEqual([['a', '<|endoftext|>', 'cc'], ['a', 'cc'], ['next']])
        expect(javaScript).toHaveBeenCalledExactlyOnceWith('<|endoftext|>')
        expect(isNativeTokenizerUnavailable('cl100k_base')).toBe(false)
    })

    it('halves a batch whose response the command refuses, down to one text in JavaScript', async () => {
        const { nativeTokenizerIds, isNativeTokenizerUnavailable } = await load()
        harness.invoke.mockImplementation(async (command: string, args: { request: NativeRequest }) => {
            if (args.request.texts.includes('many')) {
                throw { code: 'response_id_limit_exceeded', message: 'synthetic ID cap' }
            }
            return nativeAnswer(command, args)
        })

        const ids = await nativeTokenizerIds('glm5', ['a', 'b', 'many', 'c'], javaScript)

        expect(ids.map(item => item.length)).toEqual([1, 1, 40, 1])
        expect(sent()).toEqual([['a', 'b', 'many', 'c'], ['a', 'b'], ['many', 'c'], ['many'], ['c']])
        expect(javaScript).toHaveBeenCalledExactlyOnceWith('many')
        expect(isNativeTokenizerUnavailable('glm5')).toBe(false)
    })

    it.each([
        ['a missing artifact', { code: 'tokenizer_artifact', message: 'synthetic missing artifact' }],
        ['a failed worker', { code: 'tokenizer_worker_failed', message: 'synthetic worker failure' }],
        ['an IPC error', new Error('synthetic IPC failure')],
        ['a missing command', 'Command tokenize_batch not found'],
    ])('counts the family in JavaScript for the session after %s', async (_name, failure) => {
        const { nativeTokenizerCounts, isNativeTokenizerUnavailable } = await load()
        harness.invoke.mockRejectedValueOnce(failure)

        await expect(nativeTokenizerCounts('mistral', ['ab', 'c'], javaScript)).resolves.toEqual([20, 10])
        await expect(nativeTokenizerCounts('mistral', ['later'], javaScript)).resolves.toEqual([50])
        await expect(nativeTokenizerCounts('cl100k_base', ['other'], javaScript)).resolves.toEqual([5])

        expect(isNativeTokenizerUnavailable('mistral')).toBe(true)
        expect(sent()).toEqual([['ab', 'c'], ['other']])
    })

    it('leaves a request counted ahead unanswered when the native call fails, and counts the rest of its turn in JavaScript', async () => {
        const { nativeTokenizerCounts, nativeTokenizerCountsAhead } = await load()
        harness.invoke.mockRejectedValueOnce(new Error('synthetic IPC failure'))

        const [ahead, counted] = await Promise.all([
            nativeTokenizerCountsAhead('cl100k_base', ['ahead']),
            nativeTokenizerCounts('cl100k_base', ['ab'], javaScript),
        ])

        expect(ahead).toEqual([undefined])
        expect(counted).toEqual([20])
        expect(sent()).toEqual([['ahead', 'ab']])
        expect(javaScript).toHaveBeenCalledExactlyOnceWith('ab')
    })

    it('rejects a request with the JavaScript error of a text neither side counts', async () => {
        const { nativeTokenizerCounts } = await load()
        harness.invoke.mockRejectedValueOnce({ code: 'disallowed_special_token', message: 'synthetic refusal', index: 0 })
        const failure = new Error('synthetic JavaScript failure')

        const results = await Promise.allSettled([
            nativeTokenizerCounts('cl100k_base', ['<|endoftext|>'], async () => { throw failure }),
            nativeTokenizerCounts('cl100k_base', ['fine'], javaScript),
        ])

        expect(results).toEqual([{ status: 'rejected', reason: failure }, { status: 'fulfilled', value: [4] }])
    })
})
