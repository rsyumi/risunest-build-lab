/** Which chat variables a display row read, so a variable change refreshes only the rows that read it. */

export interface ChatVariableReadRecorder {
    note(key: string): void
    markUntracked(): void
}

/** The chat variables one render of a row read, or untracked when a stage it ran can read without notice. */
export class DisplayReadRecord implements ChatVariableReadRecorder {
    readonly keys = new Set<string>()
    untracked = false

    note(key: string): void {
        this.keys.add(key)
    }

    markUntracked(): void {
        this.untracked = true
    }
}

export type ScriptDisplayRefreshRequest = { keys: readonly string[] } | { all: true }

const scopes: ChatVariableReadRecorder[] = []
const ambientRecorders = new Set<ChatVariableReadRecorder>()

export function noteChatVariableRead(key: string): void {
    const scope = scopes[scopes.length - 1]
    if (scope) {
        scope.note(key)
        return
    }
    for (const recorder of ambientRecorders) recorder.note(key)
}

/** A dependency outside chat variables requires conservative row refreshes. */
export function noteUntrackedVariableRead(): void {
    const scope = scopes[scopes.length - 1]
    if (scope) {
        scope.markUntracked()
        return
    }
    for (const recorder of ambientRecorders) recorder.markUntracked()
}

/** Records the chat variables `read` reads synchronously. */
export function recordChatVariableReads<T>(
    recorder: ChatVariableReadRecorder | undefined,
    read: () => T,
): T {
    if (!recorder) return read()
    scopes.push(recorder)
    try {
        return read()
    } finally {
        scopes.pop()
    }
}

/**
 * Records reads made outside any synchronous scope until the returned function runs.
 * For an asynchronous stage that runs alone, such as the live display pipeline; reads
 * by other code while it waits only add rows to a refresh.
 */
export function recordAmbientChatVariableReads(recorder: ChatVariableReadRecorder): () => void {
    ambientRecorders.add(recorder)
    return () => {
        ambientRecorders.delete(recorder)
    }
}

function recordMatches(record: DisplayReadRecord, request: ScriptDisplayRefreshRequest): boolean {
    if (record.untracked) return true
    if ('all' in request) return record.keys.size > 0
    return request.keys.some((key) => record.keys.has(key))
}

export interface DisplayRowReads {
    /** Replaces the record of one part of the row, such as its message text or body. */
    track(part: string, record: DisplayReadRecord): void
    dispose(): void
}

interface RegisteredRow {
    index: () => number
    parts: Map<string, DisplayReadRecord>
}

const rows = new Set<RegisteredRow>()

/** Registers a mounted row; `index` returns the message index its reload pointer uses. */
export function registerDisplayRow(index: () => number): DisplayRowReads {
    const row: RegisteredRow = { index, parts: new Map() }
    rows.add(row)
    return {
        track: (part, record) => {
            row.parts.set(part, record)
        },
        dispose: () => {
            rows.delete(row)
        },
    }
}

/** The message indexes of mounted rows a refresh request reaches. A row with no record yet is included. */
export function displayRowsToRefresh(request: ScriptDisplayRefreshRequest): number[] {
    const indexes = new Set<number>()
    for (const row of rows) {
        if (row.parts.size === 0 || [...row.parts.values()].some((record) => recordMatches(record, request))) {
            indexes.add(row.index())
        }
    }
    return [...indexes]
}
