import type { NativeTokenizeBatchResponse, NativeTokenizeMode } from './tokenizer/nativeTokenizer'

type TokenizerModule = typeof import('./tokenizer')

/**
 * A tokenizer stand-in for screens that only need its import graph cut. countTogether starts every count at once, as
 * it does where the native tokenizer counts. Overrides replace single exports and must name real ones.
 */
export function tokenizerStubModule(overrides: { [K in keyof TokenizerModule]?: unknown } = {}) {
    const stub = {
        countTogether: (async (...tasks: (() => Promise<unknown>)[]) =>
            await Promise.all(tasks.map((task) => task()))) as TokenizerModule['countTogether'],
    } satisfies Partial<TokenizerModule>
    return { ...stub, ...overrides }
}

/**
 * What the native tokenizer answers a `tokenize_batch` request with: a text's count is its length, and its IDs count up
 * from 900,000, so neither matches a JavaScript tokenizer.
 */
export function nativeTokenizeAnswer(
    request: { artifact_fingerprint: string; mode: NativeTokenizeMode; texts: string[] },
): NativeTokenizeBatchResponse {
    const { mode, artifact_fingerprint, texts } = request
    return mode === 'count'
        ? { mode, artifact_fingerprint, counts: texts.map((text) => text.length) }
        : { mode, artifact_fingerprint, ids: texts.map(nativeTokenIds) }
}

/** The IDs nativeTokenizeAnswer gives `text`. */
export function nativeTokenIds(text: string): number[] {
    return Array.from(text, (_, index) => 900_000 + index)
}
