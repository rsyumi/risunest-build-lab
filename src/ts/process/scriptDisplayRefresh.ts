import { ReloadChatPointer, ScriptDisplayRefreshPointer } from '../stores.svelte'
import { displayRowsToRefresh, type ScriptDisplayRefreshRequest } from './scriptVariableReads'

let pending: { all: boolean, keys: Set<string> } | null = null

/**
 * Re-renders the chat rows that read what a script changed: the changed chat variables,
 * or any chat variable for an explicit `reloadDisplay`. Requests in one task coalesce.
 */
export function requestScriptDisplayRefresh(request: ScriptDisplayRefreshRequest): void {
    if (!('all' in request) && request.keys.length === 0) return
    if (!pending) {
        pending = { all: false, keys: new Set() }
        queueMicrotask(flushScriptDisplayRefresh)
    }
    if ('all' in request) pending.all = true
    else for (const key of request.keys) pending.keys.add(key)
}

function flushScriptDisplayRefresh(): void {
    const request = pending
    pending = null
    if (!request) return
    const indexes = displayRowsToRefresh(request.all ? { all: true } : { keys: [...request.keys] })
    // Clears cached regex results and refreshes the background before rows render again.
    ScriptDisplayRefreshPointer.update((value) => value + 1)
    if (indexes.length === 0) return
    ReloadChatPointer.update((pointers) => {
        for (const index of indexes) pointers[index] = (pointers[index] ?? 0) + 1
        return pointers
    })
}
