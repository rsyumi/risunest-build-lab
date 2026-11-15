import {
    invokeNativeTokenizerBatch,
    isNativeModelTokenizerId,
    resolveNativeTokenizerRoute,
    type NativeModelTokenizerId,
    type NativeTokenizerId,
    type NativeTokenizerInvoke,
} from './nativeTokenizer'

export const NATIVE_TOKENIZER_MAX_BATCH_ITEMS = 1_000
export const NATIVE_TOKENIZER_MAX_AGGREGATE_INPUT_BYTES = 1_048_576

export type NativeTokenizerIdsBatchCandidate = {
    itemCount: number
    aggregateInputBytes: () => number
    buildTexts: () => string[]
}

export type ProductionNativeTokenizerId = NativeTokenizerId | NativeModelTokenizerId

export type ProductionNativeTokenizerContext = {
    isTauri: boolean
    aiModel: string
    customTokenizer: string
    modelTokenizerId: ProductionNativeTokenizerId | null
    pluginTokenizer?: string
}

function isNativeTokenizerId(tokenizerId?: string): tokenizerId is NativeTokenizerId {
    return tokenizerId === 'cl100k_base' || tokenizerId === 'o200k_base'
}

// The native tokenizer for the one `encode` runs, or null to keep JavaScript: the web build, a plugin's own tokenizer,
// a family without a native artifact, and novellist, whose native encoder is still several times slower than
// web-tokenizers.
export function resolveProductionNativeTokenizerId(
    context: ProductionNativeTokenizerContext,
): ProductionNativeTokenizerId | null {
    if (!context.isTauri) {
        return null
    }
    const tokenizerId = encodeTokenizerId(context)
    return tokenizerId === 'novellist' ? null : tokenizerId
}

// The tokenizer `encode` selects, branch for branch; an unrecognized name selects o200k_base there too.
function encodeTokenizerId(context: ProductionNativeTokenizerContext): ProductionNativeTokenizerId | null {
    if (context.aiModel === 'openrouter' || context.aiModel === 'reverse_proxy') {
        return isNativeModelTokenizerId(context.customTokenizer) ? context.customTokenizer : 'o200k_base'
    }
    const pluginTokenizer = context.pluginTokenizer ?? 'none'
    if (context.aiModel === 'custom' && pluginTokenizer) {
        if (pluginTokenizer === 'custom') {
            return null
        }
        return isNativeTokenizerId(pluginTokenizer) || isNativeModelTokenizerId(pluginTokenizer)
            ? pluginTokenizer
            : 'o200k_base'
    }
    return context.modelTokenizerId
}

// Families whose native tokenizer failed in a way the next call would repeat: a missing or declined artifact, a failed
// worker, an IPC error. They count in JavaScript for the rest of the session.
const unavailableTokenizers = new Set<ProductionNativeTokenizerId>()

export function isNativeTokenizerUnavailable(tokenizerId: ProductionNativeTokenizerId): boolean {
    return unavailableTokenizers.has(tokenizerId)
}

export function markNativeTokenizerUnavailable(tokenizerId: ProductionNativeTokenizerId): void {
    unavailableTokenizers.add(tokenizerId)
}

// Refusals of a batch's size or response rather than of the tokenizer: a smaller batch can still be counted natively.
const BATCH_REFUSALS = new Set([
    'batch_item_limit_exceeded',
    'aggregate_input_limit_exceeded',
    'response_id_limit_exceeded',
    'token_count_overflow',
    'lone_surrogate',
])

export type NativeTokenizerFailure =
    // One text the tokenizer refuses (a special token, a regex or input limit): JavaScript counts that text only.
    | { kind: 'text'; index: number }
    // The batch as a whole: a smaller batch, down to a single text in JavaScript.
    | { kind: 'batch' }
    // The tokenizer itself: JavaScript for the family from now on.
    | { kind: 'unavailable' }

export function classifyNativeTokenizerFailure(error: unknown): NativeTokenizerFailure {
    const failure = typeof error === 'object' && error !== null ? (error as { code?: unknown; index?: unknown }) : {}
    if (typeof failure.code !== 'string') {
        return { kind: 'unavailable' }
    }
    if (typeof failure.index === 'number' && Number.isSafeInteger(failure.index) && failure.index >= 0) {
        return { kind: 'text', index: failure.index }
    }
    return BATCH_REFUSALS.has(failure.code) ? { kind: 'batch' } : { kind: 'unavailable' }
}

export async function tryNativeTokenizerIdsBatch(
    candidate: NativeTokenizerIdsBatchCandidate,
    context: ProductionNativeTokenizerContext,
    invokeCommand?: NativeTokenizerInvoke,
): Promise<number[][] | null> {
    if (candidate.itemCount > NATIVE_TOKENIZER_MAX_BATCH_ITEMS) {
        return null
    }
    const tokenizerId = resolveProductionNativeTokenizerId(context)
    if (!tokenizerId || isNativeTokenizerUnavailable(tokenizerId)) {
        return null
    }
    const route = resolveNativeTokenizerRoute(tokenizerId, context.isTauri, true)
    if (route.kind === 'existing') {
        return null
    }
    if (candidate.aggregateInputBytes() > NATIVE_TOKENIZER_MAX_AGGREGATE_INPUT_BYTES) {
        return null
    }
    const texts = candidate.buildTexts()
    try {
        const response = await invokeNativeTokenizerBatch(route, texts, 'ids', invokeCommand)
        return response.mode === 'ids' ? response.ids : null
    } catch (error) {
        if (classifyNativeTokenizerFailure(error).kind === 'unavailable') {
            markNativeTokenizerUnavailable(tokenizerId)
        }
        throw error
    }
}
