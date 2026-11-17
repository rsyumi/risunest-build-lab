import type { DeviceMarkerStorage } from './storage/deviceMarkers'

const RECORD_KEY = 'risuNestDisplayRecovery'
const MARK_LIMIT = 16
const RELEASE_DELAY_MS = 1_500
// These can arrive while the app is in the foreground and its display work continues.
const FOREGROUND_FLUSH_REASONS = new Set(['trim-memory', 'memory-warning'])

export interface DisplayRecoveryTarget {
    characterId: string
    conversationId: string
}

/** Display work for one chat. `ready` is the record write the work waits for, if any. */
export interface DisplayWork {
    readonly ready: Promise<void> | null
    release(): void
}

export type DisplayRecoveryStore = Pick<DeviceMarkerStorage, 'getItem' | 'setItem' | 'removeItem' | 'flush' | 'settled'>

export interface DisplayRecoveryTimers {
    set(callback: () => void, delayMs: number): unknown
    clear(handle: unknown): void
}

export interface DisplayRecoveryOptions {
    releaseDelayMs?: number
    markLimit?: number
    timers?: DisplayRecoveryTimers
}

export type DisplayRecovery = ReturnType<typeof createDisplayRecovery>

interface SelectedCharacter {
    chaId?: string
    chatPage?: number
    chats?: ArrayLike<{ id?: string } | undefined>
}

const idleWork: DisplayWork = { ready: null, release() {} }

function keyOf(target: DisplayRecoveryTarget): string {
    return `${target.characterId}\u0000${target.conversationId}`
}

function readTargets(value: unknown): DisplayRecoveryTarget[] {
    if (!Array.isArray(value)) return []
    return value.flatMap((entry) =>
        typeof entry?.characterId === 'string' && typeof entry?.conversationId === 'string'
            ? [{ characterId: entry.characterId, conversationId: entry.conversationId }]
            : [])
}

function readRecord(text: string | null): { working: DisplayRecoveryTarget[], marked: DisplayRecoveryTarget[] } {
    try {
        const value = text === null ? null : JSON.parse(text)
        return { working: readTargets(value?.working), marked: readTargets(value?.marked) }
    } catch {
        return { working: [], marked: [] }
    }
}

/**
 * Names the chats whose display work is running in a per-device record. A chat the record
 * still names at the next launch was open when the app ended, so it opens with Modify
 * Display turned off until the user turns it back on.
 */
export function createDisplayRecovery(store: DisplayRecoveryStore, options: DisplayRecoveryOptions = {}) {
    const releaseDelayMs = options.releaseDelayMs ?? RELEASE_DELAY_MS
    const markLimit = options.markLimit ?? MARK_LIMIT
    const timers = options.timers ?? {
        set: (callback, delayMs) => setTimeout(callback, delayMs),
        clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    }
    const initialText = store.getItem(RECORD_KEY)
    const stored = readRecord(initialText)
    const startMarks = new Map<string, DisplayRecoveryTarget>()
    for (const target of [...stored.marked, ...stored.working]) {
        startMarks.delete(keyOf(target))
        startMarks.set(keyOf(target), target)
    }
    let marked = $state.raw([...startMarks.values()].slice(-markLimit))
    const work = new Map<string, { target: DisplayRecoveryTarget, count: number, timer: unknown }>()
    const recorded = new Map<string, DisplayRecoveryTarget>()
    let suspended = false
    let storedText = initialText
    let lastWrite: Promise<void> = Promise.resolve()
    let writing = false

    function persist(): Promise<void> {
        const working = [...recorded.values()]
        const text = working.length === 0 && marked.length === 0 ? null : JSON.stringify({ working, marked })
        if (text === storedText) return lastWrite
        storedText = text
        if (text === null) store.removeItem(RECORD_KEY)
        else store.setItem(RECORD_KEY, text)
        writing = true
        // Waiting for the write must not take a failure that another caller's flush reports.
        const write: Promise<void> = (store.settled ? store.settled() : store.flush()).catch((error) => {
            console.error('The display recovery record could not be saved', error)
        }).then(() => {
            if (lastWrite === write) writing = false
        })
        lastWrite = write
        return write
    }

    function clearTimer(entry: { timer: unknown }): void {
        if (entry.timer === null) return
        timers.clear(entry.timer)
        entry.timer = null
    }

    function forget(matches: (target: DisplayRecoveryTarget) => boolean): Promise<void> {
        const next = marked.filter((target) => !matches(target))
        if (next.length === marked.length) return Promise.resolve()
        marked = next
        return persist()
    }

    return {
        /** The write that moved the last run's record into the marks, if there was one. */
        settled: persist(),

        isMarked(target: DisplayRecoveryTarget | null): boolean {
            if (!target) return false
            const key = keyOf(target)
            return marked.some((candidate) => keyOf(candidate) === key)
        },

        /**
         * Starts display work for a chat. The first work writes the record and waits for it;
         * work that starts while it is stored waits for nothing. The record is removed a
         * while after the last work of the chat ends.
         */
        beginWork(target: DisplayRecoveryTarget | null): DisplayWork {
            if (!target) return idleWork
            const key = keyOf(target)
            let entry = work.get(key)
            if (!entry) work.set(key, entry = { target, count: 0, timer: null })
            const current = entry
            current.count++
            clearTimer(current)
            let ready: Promise<void> | null = null
            if (!suspended) {
                if (!recorded.has(key)) {
                    recorded.set(key, target)
                    ready = persist()
                } else if (writing) {
                    ready = lastWrite
                }
            }
            let released = false
            return {
                ready,
                release() {
                    if (released) return
                    released = true
                    current.count--
                    if (current.count > 0) return
                    if (!recorded.has(key)) {
                        work.delete(key)
                        return
                    }
                    current.timer = timers.set(() => {
                        current.timer = null
                        if (current.count > 0) return
                        work.delete(key)
                        recorded.delete(key)
                        void persist()
                    }, releaseDelayMs)
                },
            }
        },

        /** Removes the record while the app is in the background. */
        suspend(): Promise<void> {
            suspended = true
            for (const [key, entry] of work) {
                clearTimer(entry)
                if (entry.count === 0) work.delete(key)
            }
            recorded.clear()
            return persist()
        },

        /** Writes the record again for the work that continued while the app was away. */
        resume(): void {
            if (!suspended) return
            suspended = false
            for (const [key, entry] of work) recorded.set(key, entry.target)
            void persist()
        },

        forget,
    }
}

let installed = $state.raw<DisplayRecovery | null>(null)

/** Loads the record at startup, before any display work can write it. */
export function installDisplayRecovery(store: DisplayRecoveryStore | null, options?: DisplayRecoveryOptions): Promise<void> {
    installed = store ? createDisplayRecovery(store, options) : null
    return installed?.settled ?? Promise.resolve()
}

/**
 * The selected chat, when `characterId` is its character or is omitted. The selection is
 * read only while the record is in use.
 */
export function displayRecoveryTarget(
    selected: () => SelectedCharacter | null | undefined,
    characterId?: string,
): DisplayRecoveryTarget | null {
    if (!installed) return null
    try {
        const character = selected()
        const conversationId = character?.chats?.[character.chatPage ?? 0]?.id
        if (!character?.chaId || typeof conversationId !== 'string') return null
        if (characterId !== undefined && characterId !== character.chaId) return null
        return { characterId: character.chaId, conversationId }
    } catch {
        return null
    }
}

export function isDisplayRecoveryMarked(target: DisplayRecoveryTarget | null): boolean {
    return installed?.isMarked(target) ?? false
}

export function beginDisplayWork(target: DisplayRecoveryTarget | null): DisplayWork {
    return installed?.beginWork(target) ?? idleWork
}

/** Runs `work` as display work of `target`, after its record is stored. */
export async function withDisplayWork<T>(target: DisplayRecoveryTarget | null, work: () => Promise<T>): Promise<T> {
    const displayWork = beginDisplayWork(target)
    try {
        if (displayWork.ready) await displayWork.ready
        return await work()
    } finally {
        displayWork.release()
    }
}

/** Part of the lifecycle flush, so the record is gone before the app may be stopped. */
export function suspendDisplayWork(reason: string): Promise<void> {
    if (!installed) return Promise.resolve()
    if (FOREGROUND_FLUSH_REASONS.has(reason) && typeof document !== 'undefined' && document.visibilityState !== 'hidden') {
        return Promise.resolve()
    }
    return installed.suspend()
}

/** Writes the record again once the app is back in the foreground. */
export function listenForDisplayWorkResume(): () => void {
    const resume = () => {
        if (document.visibilityState !== 'hidden') installed?.resume()
    }
    window.addEventListener('focus', resume)
    window.addEventListener('pageshow', resume)
    window.addEventListener('pointerdown', resume, { capture: true, passive: true })
    document.addEventListener('visibilitychange', resume)
    return () => {
        window.removeEventListener('focus', resume)
        window.removeEventListener('pageshow', resume)
        window.removeEventListener('pointerdown', resume, { capture: true })
        document.removeEventListener('visibilitychange', resume)
    }
}

/** Drops the marks of a chat turned back on, or of deleted chats or characters. */
export function forgetDisplayRecovery(matches: (target: DisplayRecoveryTarget) => boolean): Promise<void> {
    return installed?.forget(matches) ?? Promise.resolve()
}
