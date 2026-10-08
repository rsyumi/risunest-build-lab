const listeners = new Set<() => void>()

/** Runs `listener` each time a sync target change settles, whether it bound, unbound, failed or was cancelled. */
export function subscribeSyncBindingChanges(listener: () => void): () => void {
    listeners.add(listener)
    return () => { listeners.delete(listener) }
}

export function notifySyncBindingChanged(): void {
    for (const listener of [...listeners]) {
        try { listener() } catch { /* One view that fails to update does not stop the others. */ }
    }
}
