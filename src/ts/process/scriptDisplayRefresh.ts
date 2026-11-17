import { ReloadChatPointer, ScriptDisplayRebuildPointer, ScriptDisplayRefreshPointer } from '../stores.svelte'
import { displayRowsToRefresh, markDisplayRowsForRebuild, type ScriptDisplayRefreshRequest } from './scriptVariableReads'

let pending: { all: boolean, keys: Set<string> } | null = null
let deferRequest: ((request: ScriptDisplayRefreshRequest) => void) | undefined
let deferRow: ((index: number | string) => void) | undefined

export function createScriptDisplayRefreshBatch() {
    let all = false
    const keys = new Set<string>()
    const indexes = new Map<string, number | string>()
    return {
        run<T>(callback: () => T): T {
            const previous = deferRequest
            const previousRow = deferRow
            deferRow = index => { indexes.set(String(index), index) }
            deferRequest = request => {
                if ('all' in request) all = true
                else for (const key of request.keys) keys.add(key)
            }
            try { return callback() }
            finally { deferRequest = previous; deferRow = previousRow }
        },
        flush(): void {
            if (all) requestScriptDisplayRefresh({ all: true })
            else if (keys.size) requestScriptDisplayRefresh({ keys: [...keys] })
            for (const index of indexes.values()) markDisplayRowsForRebuild(index)
            bumpReloadPointers([...indexes.values()])
            all = false
            keys.clear()
            indexes.clear()
        },
    }
}

/**
 * Re-renders the chat rows that read the chat variables a script changed. An explicit
 * `reloadDisplay` re-renders every mounted row and the background, rebuilding their DOM.
 * Requests in one task coalesce.
 */
export function requestScriptDisplayRefresh(request: ScriptDisplayRefreshRequest): void {
    if (deferRequest) { deferRequest(request); return }
    if (!('all' in request) && request.keys.length === 0) return
    if (!pending) {
        pending = { all: false, keys: new Set() }
        queueMicrotask(flushScriptDisplayRefresh)
    }
    if ('all' in request) pending.all = true
    else for (const key of request.keys) pending.keys.add(key)
}

/**
 * Re-renders the mounted rows at a message index now and rebuilds their DOM, for a script reload
 * of that message. Scripts may pass the index as a string, which is used as the reload pointer key.
 */
export function rebuildScriptDisplayRow(index: number | string): void {
    if (deferRow) { deferRow(index); return }
    markDisplayRowsForRebuild(index)
    bumpReloadPointers([index])
}

function flushScriptDisplayRefresh(): void {
    const request = pending
    pending = null
    if (!request) return
    const indexes = request.all ? markDisplayRowsForRebuild() : displayRowsToRefresh({ keys: [...request.keys] })
    if (request.all) ScriptDisplayRebuildPointer.update((value) => value + 1)
    // Clears cached regex results and refreshes the background before rows render again.
    ScriptDisplayRefreshPointer.update((value) => value + 1)
    bumpReloadPointers(indexes)
}

function bumpReloadPointers(indexes: Array<number | string>): void {
    if (indexes.length === 0) return
    ReloadChatPointer.update((pointers) => {
        for (const index of indexes) pointers[index] = (pointers[index] ?? 0) + 1
        return pointers
    })
}
