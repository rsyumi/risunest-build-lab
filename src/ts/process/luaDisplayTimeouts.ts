const ENGINE_LIMIT = 64
const ROW_FAILURE_LIMIT = 256

let lastOrder = 0
const skipThrough = new Map<string, number>()
const rowFailures = new Set<string>()

/** The chat row a display listener renders. */
export interface DisplayListenerRow {
    conversationId: string
    index: number
    messageId: string
}

export interface DisplayListenerRequest {
    /** When the view asked for this display, from `nextDisplayListenerOrder`. */
    order: number
    row?: DisplayListenerRow
}

/** Orders a display request at the moment the view asks for it, before it queues for scripts. */
export function nextDisplayListenerOrder(): number {
    return ++lastOrder
}

/** Identifies a Lua engine by its owner and code, so a recreated engine keeps its failures. */
export function displayListenerEngine(ownerId: string, code: string): string {
    return hash(`${ownerId}\u0000${code}`)
}

/**
 * A Lua display listener that timed out is skipped by the requests made before the timeout,
 * and by later requests for the same row and input.
 */
export function skipsDisplayListener(engine: string, request: DisplayListenerRequest, input: string): boolean {
    if (request.order <= (skipThrough.get(engine) ?? 0)) return true
    const key = rowFailureKey(engine, request, input)
    if (key === null || !rowFailures.has(key)) return false
    remember(rowFailures, key, ROW_FAILURE_LIMIT)
    return true
}

export function noteDisplayListenerTimeout(engine: string, request: DisplayListenerRequest, input: string): void {
    skipThrough.delete(engine)
    skipThrough.set(engine, lastOrder)
    while (skipThrough.size > ENGINE_LIMIT) skipThrough.delete(skipThrough.keys().next().value!)
    const key = rowFailureKey(engine, request, input)
    if (key !== null) remember(rowFailures, key, ROW_FAILURE_LIMIT)
}

function rowFailureKey(engine: string, request: DisplayListenerRequest, input: string): string | null {
    const row = request.row
    if (!row) return null
    return `${engine}:${hash(`${row.conversationId}\u0000${row.index}\u0000${row.messageId}\u0000${input}`)}`
}

function remember(entries: Set<string>, key: string, limit: number): void {
    entries.delete(key)
    entries.add(key)
    while (entries.size > limit) entries.delete(entries.values().next().value!)
}

// cyrb53: a fast 53-bit string hash. A collision only skips one listener for one row.
function hash(value: string): string {
    let h1 = 0xdeadbeef
    let h2 = 0x41c6ce57
    for (let i = 0; i < value.length; i++) {
        const code = value.charCodeAt(i)
        h1 = Math.imul(h1 ^ code, 2654435761)
        h2 = Math.imul(h2 ^ code, 1597334677)
    }
    h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909)
    h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909)
    return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(36)
}
