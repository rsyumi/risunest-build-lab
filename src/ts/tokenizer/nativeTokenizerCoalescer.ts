import { hasLoneSurrogate, invokeNativeTokenizerBatch, resolveNativeTokenizerRoute } from './nativeTokenizer'
import {
    NATIVE_TOKENIZER_MAX_AGGREGATE_INPUT_BYTES,
    NATIVE_TOKENIZER_MAX_BATCH_ITEMS,
    classifyNativeTokenizerFailure,
    isNativeTokenizerUnavailable,
    markNativeTokenizerUnavailable,
    type ProductionNativeTokenizerId,
} from './nativeTokenizerProduction'

// The UTF-8 one invoke carries: a few invokes of this size cost less than one of the command's 1 MiB.
export const NATIVE_TOKENIZER_COALESCED_MAX_BYTES = 262_144

export type TokenIds = number[] | Uint32Array | Int32Array
// Encodes one text in JavaScript, for a text the native tokenizer refuses or a family it cannot load.
export type NativeTokenizerFallback = (text: string) => Promise<TokenIds>

type Mode = 'count' | 'ids'
type Request = {
    texts: string[]
    // Null for texts counted ahead, which stay undefined unless the native tokenizer answers them.
    fallback: NativeTokenizerFallback | null
    // A count or IDs per text; undefined until the native tokenizer answers, and left so for JavaScript.
    results: (number | TokenIds | undefined)[]
    resolve: (results: (number | TokenIds | undefined)[]) => void
    reject: (error: unknown) => void
}
type Queue = { tokenizerId: ProductionNativeTokenizerId; mode: Mode; requests: Request[] }
type Item = { request: Request; index: number; text: string; bytes: number }

const queues = new Map<string, Queue>()
let flushScheduled = false
let flushes: Promise<void> = Promise.resolve()

// The token count of each text. Texts requested in the same turn go out together, in as few invokes as the limits allow.
export function nativeTokenizerCounts(
    tokenizerId: ProductionNativeTokenizerId,
    texts: string[],
    fallback: NativeTokenizerFallback,
): Promise<number[]> {
    return enqueue(tokenizerId, 'count', texts, fallback) as Promise<number[]>
}

// The count of each text the native tokenizer answers, coalesced as counts are, and undefined for the rest, which
// nothing counts in JavaScript: a walk counted ahead counts those texts only if it reaches them.
export function nativeTokenizerCountsAhead(
    tokenizerId: ProductionNativeTokenizerId,
    texts: string[],
): Promise<(number | undefined)[]> {
    return enqueue(tokenizerId, 'count', texts, null) as Promise<(number | undefined)[]>
}

// The token IDs of each text, coalesced as counts are.
export function nativeTokenizerIds(
    tokenizerId: ProductionNativeTokenizerId,
    texts: string[],
    fallback: NativeTokenizerFallback,
): Promise<TokenIds[]> {
    return enqueue(tokenizerId, 'ids', texts, fallback) as Promise<TokenIds[]>
}

function enqueue(tokenizerId: ProductionNativeTokenizerId, mode: Mode, texts: string[], fallback: NativeTokenizerFallback | null) {
    if (texts.length === 0) return Promise.resolve([])
    return new Promise<(number | TokenIds | undefined)[]>((resolve, reject) => {
        const key = `${mode}:${tokenizerId}`
        let queue = queues.get(key)
        if (!queue) {
            queue = { tokenizerId, mode, requests: [] }
            queues.set(key, queue)
        }
        queue.requests.push({ texts: [...texts], fallback, results: new Array(texts.length), resolve, reject })
        if (!flushScheduled) {
            flushScheduled = true
            // One flush at a time, so requests made while an invoke runs join the next batch.
            flushes = flushes.then(flush)
        }
    })
}

async function flush() {
    flushScheduled = false
    const pending = [...queues.values()]
    queues.clear()
    // In turn: the JavaScript tokenizers keep one loaded model, which concurrent loads would replace under each other.
    for (const queue of pending) await settleQueue(queue)
}

async function settleQueue(queue: Queue) {
    try {
        await countNatively(queue)
    } catch {
        // Whatever the native pass left unanswered is counted in JavaScript below.
    }
    for (const request of queue.requests) {
        try {
            for (const [index, text] of request.texts.entries()) {
                if (request.results[index] !== undefined || request.fallback === null) continue
                const ids = await request.fallback(text)
                request.results[index] = queue.mode === 'count' ? ids.length : ids
            }
            request.resolve(request.results)
        } catch (error) {
            request.reject(error)
        }
    }
}

async function countNatively(queue: Queue) {
    const route = resolveNativeTokenizerRoute(queue.tokenizerId, true, true)
    if (route.kind === 'existing') return
    const items: Item[] = []
    for (const request of queue.requests) {
        for (const [index, text] of request.texts.entries()) {
            const bytes = utf8ByteLength(text)
            // Text the command refuses in any batch: past its input limit, or a lone surrogate a model family would read
            // differently from web-tokenizers. It goes to JavaScript without an invoke.
            if (bytes > NATIVE_TOKENIZER_MAX_AGGREGATE_INPUT_BYTES
                || (route.kind === 'native-model' && hasLoneSurrogate(text))) continue
            items.push({ request, index, text, bytes })
        }
    }
    const batches = splitItems(items)
    while (batches.length > 0 && !isNativeTokenizerUnavailable(queue.tokenizerId)) {
        const batch = batches.shift()!
        try {
            const response = await invokeNativeTokenizerBatch(route, batch.map(item => item.text), queue.mode)
            const values = response.mode === 'count' ? response.counts : response.ids
            batch.forEach((item, position) => { item.request.results[item.index] = values[position] })
        } catch (error) {
            const failure = classifyNativeTokenizerFailure(error)
            if (failure.kind === 'unavailable') {
                markNativeTokenizerUnavailable(queue.tokenizerId)
            } else if (failure.kind === 'text' && failure.index < batch.length) {
                // The refused text goes to JavaScript; the rest of the batch is sent again.
                const rest = batch.filter((_, position) => position !== failure.index)
                if (rest.length > 0) batches.unshift(rest)
            } else if (batch.length > 1) {
                // A refusal of the batch as a whole, such as more IDs than one response holds: halve it until the
                // single text it leaves goes to JavaScript.
                const half = Math.ceil(batch.length / 2)
                batches.unshift(batch.slice(0, half), batch.slice(half))
            }
        }
    }
}

// Consecutive runs of at most NATIVE_TOKENIZER_MAX_BATCH_ITEMS texts and NATIVE_TOKENIZER_COALESCED_MAX_BYTES of UTF-8;
// a longer text goes alone.
function splitItems(items: Item[]): Item[][] {
    const batches: Item[][] = []
    let batch: Item[] = []
    let bytes = 0
    for (const item of items) {
        if (batch.length > 0
            && (batch.length >= NATIVE_TOKENIZER_MAX_BATCH_ITEMS || bytes + item.bytes > NATIVE_TOKENIZER_COALESCED_MAX_BYTES)) {
            batches.push(batch)
            batch = []
            bytes = 0
        }
        batch.push(item)
        bytes += item.bytes
    }
    if (batch.length > 0) batches.push(batch)
    return batches
}

// The UTF-8 length the command measures, with a lone surrogate counted as the U+FFFD it becomes.
function utf8ByteLength(text: string): number {
    let bytes = 0
    for (let index = 0; index < text.length; index++) {
        const unit = text.charCodeAt(index)
        if (unit < 0x80) bytes += 1
        else if (unit < 0x800) bytes += 2
        else if (unit >= 0xd800 && unit <= 0xdbff && (text.charCodeAt(index + 1) & 0xfc00) === 0xdc00) {
            bytes += 4
            index++
        } else bytes += 3
    }
    return bytes
}
