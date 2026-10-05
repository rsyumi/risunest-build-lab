export type ChatViewRole = 'user' | 'char'

export interface ChatViewRow {
    index: number
    messageId: string | null
    role: ChatViewRole
}

export interface ChatViewConversation {
    characterId: string | null
    conversationId: string | null
    characterIndex: number
    chatIndex: number
}

export type ChatViewEvent =
    | ({ type: 'conversation' } & ChatViewConversation)
    | {
          type: 'rows'
          characterId: string
          conversationId: string
          mounted: ChatViewRow[]
          unmounted: ChatViewRow[]
          rerendered: ChatViewRow[]
      }

export interface ChatViewRowReport {
    characterId: string
    conversationId: string | undefined
    index: number
    message: { role: string; chatId?: string }
    /** The reply is still arriving; its renders are not reported until it is final. */
    streaming: boolean
}

/** What one chat view reports about its mounted message rows, keyed by its own row keys. */
export interface ChatViewReporter {
    /** The row's content was rendered, by a first mount, a remount or an in-place refresh. */
    rendered(key: string, report: ChatViewRowReport): void
    /** A mounted row was kept without rendering its content again. */
    updated(key: string, report: ChatViewRowReport): void
    /** A mounted row rendered its content again by itself. */
    contentRendered(key: string): void
    /** A mounted row kept its DOM under a new key. */
    moved(previousKey: string, nextKey: string): void
    removed(key: string): void
    dispose(): void
}

export interface ChatViewListenerAccess {
    register(callback: (event: ChatViewEvent) => unknown): { id: string }
    unregister(id: string): void
    dispose(): void
}

export interface ChatViewEventDependencies {
    /** Null while the selected conversation is still being resolved; nothing is reported until it is. */
    readConversation(): ChatViewConversation | null
    /** Calls `onChange` whenever `readConversation` may return something else; returns a stop function. */
    watchConversation(onChange: () => void): () => void
    requestFrame(callback: () => void): void
    createId(): string
}

interface MountedRow {
    characterId: string
    conversationId: string | undefined
    row: ChatViewRow
    streaming: boolean
}

interface Listener {
    owner: string
    callback: (event: ChatViewEvent) => unknown
    conversation?: string
    reported: Map<string, ChatViewRow>
}

function toRow(report: ChatViewRowReport): ChatViewRow | null {
    const { role, chatId } = report.message
    if (!report.conversationId || (role !== 'user' && role !== 'char')) return null
    return { index: report.index, messageId: chatId ?? null, role }
}

const sameRow = (left: ChatViewRow, right: ChatViewRow) =>
    left.index === right.index && left.messageId === right.messageId && left.role === right.role

export function createChatViewEvents(dependencies: ChatViewEventDependencies) {
    const rows = new Map<string, MountedRow>()
    const rerendered = new Set<string>()
    const listeners = new Map<string, Listener>()
    let reporterCount = 0
    let frameQueued = false
    let stopWatching: (() => void) | null = null

    function schedule(): void {
        if (frameQueued || listeners.size === 0) return
        frameQueued = true
        dependencies.requestFrame(flush)
    }

    function emit(listener: Listener, event: ChatViewEvent): void {
        try {
            Promise.resolve(listener.callback(event)).catch((error) => console.error(error))
        } catch (error) {
            console.error(error)
        }
    }

    function flush(): void {
        frameQueued = false
        if (listeners.size === 0) {
            rerendered.clear()
            return
        }
        const conversation = dependencies.readConversation()
        if (!conversation) return
        const identity = JSON.stringify([
            conversation.characterId,
            conversation.conversationId,
            conversation.characterIndex,
            conversation.chatIndex,
        ])
        const { characterId, conversationId } = conversation
        const current = new Map<string, ChatViewRow>()
        if (characterId !== null && conversationId !== null) {
            for (const [key, entry] of rows) {
                if (entry.characterId === characterId && entry.conversationId === conversationId) {
                    current.set(key, entry.row)
                }
            }
        }
        for (const listener of [...listeners.values()]) {
            if (listener.conversation !== identity) {
                listener.conversation = identity
                listener.reported = new Map()
                emit(listener, { type: 'conversation', ...conversation })
            }
            if (characterId === null || conversationId === null) continue
            const mounted: ChatViewRow[] = []
            const unmounted: ChatViewRow[] = []
            const refreshed: ChatViewRow[] = []
            for (const [key, row] of listener.reported) {
                if (!current.has(key)) unmounted.push({ ...row })
            }
            for (const [key, row] of current) {
                const previous = listener.reported.get(key)
                if (!previous) {
                    mounted.push({ ...row })
                } else if (!sameRow(previous, row)) {
                    unmounted.push({ ...previous })
                    mounted.push({ ...row })
                } else if (rerendered.has(key)) {
                    refreshed.push({ ...row })
                }
            }
            listener.reported = new Map(current)
            if (mounted.length || unmounted.length || refreshed.length) {
                emit(listener, { type: 'rows', characterId, conversationId, mounted, unmounted, rerendered: refreshed })
            }
        }
        rerendered.clear()
    }

    function record(key: string, report: ChatViewRowReport, rendered: boolean): void {
        const row = toRow(report)
        const previous = rows.get(key)
        if (!row) {
            if (previous) remove(key)
            return
        }
        if (!previous && !rendered) return
        if (
            listeners.size > 0 &&
            previous &&
            (rendered ? !report.streaming : previous.streaming && !report.streaming)
        ) {
            rerendered.add(key)
        }
        rows.set(key, {
            characterId: report.characterId,
            conversationId: report.conversationId,
            row,
            streaming: report.streaming,
        })
        schedule()
    }

    function remove(key: string): void {
        if (!rows.delete(key)) return
        // A row mounted again within the same frame lost its decorations.
        if (listeners.size > 0) rerendered.add(key)
        schedule()
    }

    function markRendered(key: string): void {
        const entry = rows.get(key)
        if (!entry || entry.streaming || listeners.size === 0) return
        rerendered.add(key)
        schedule()
    }

    function move(previousKey: string, nextKey: string): void {
        const entry = rows.get(previousKey)
        if (!entry || previousKey === nextKey) return
        rows.delete(previousKey)
        rows.set(nextKey, entry)
        if (rerendered.delete(previousKey)) rerendered.add(nextKey)
        for (const listener of listeners.values()) {
            const reported = listener.reported.get(previousKey)
            if (!reported) continue
            listener.reported.delete(previousKey)
            listener.reported.set(nextKey, reported)
        }
    }

    function unregister(id: string, owner?: string): void {
        const listener = listeners.get(id)
        if (!listener || (owner !== undefined && listener.owner !== owner)) return
        listeners.delete(id)
        if (listeners.size === 0) {
            stopWatching?.()
            stopWatching = null
            rerendered.clear()
            // A pending frame finds nobody to report to; the next listener asks for its own.
            frameQueued = false
        }
    }

    return {
        createReporter(): ChatViewReporter {
            const prefix = `${++reporterCount}:`
            let disposed = false
            return {
                rendered: (key, report) => {
                    if (!disposed) record(prefix + key, report, true)
                },
                updated: (key, report) => {
                    if (!disposed) record(prefix + key, report, false)
                },
                contentRendered: (key) => markRendered(prefix + key),
                moved: (previousKey, nextKey) => move(prefix + previousKey, prefix + nextKey),
                removed: (key) => remove(prefix + key),
                dispose() {
                    disposed = true
                    for (const key of [...rows.keys()]) {
                        if (key.startsWith(prefix)) remove(key)
                    }
                },
            }
        },
        forOwner(owner: string): ChatViewListenerAccess {
            return {
                register(callback) {
                    const id = dependencies.createId()
                    listeners.set(id, { owner, callback, reported: new Map() })
                    stopWatching ??= dependencies.watchConversation(schedule)
                    schedule()
                    return { id }
                },
                unregister: (id) => unregister(id, owner),
                dispose() {
                    for (const [id, listener] of [...listeners]) {
                        if (listener.owner === owner) unregister(id)
                    }
                },
            }
        },
    }
}

const unselected: ChatViewConversation = { characterId: null, conversationId: null, characterIndex: -1, chatIndex: -1 }

export interface PinnedChatViewConversationDependencies {
    /** The selection in the working set; its indices only tell when it moved. */
    readSelection(): ChatViewConversation
    watchSelection(onChange: () => void): () => void
    resolvePosition(characterId: string, conversationId: string | null): Promise<{ characterIndex: number; chatIndex: number }>
}

/** Reports the selected conversation at the position the index APIs use for it. */
export function createPinnedChatViewConversation(
    dependencies: PinnedChatViewConversationDependencies,
): Pick<ChatViewEventDependencies, 'readConversation' | 'watchConversation'> {
    let current: ChatViewConversation | null = null
    let requested: string | undefined
    let request = 0

    function refresh(onChange: () => void): void {
        const selection = dependencies.readSelection()
        const key = JSON.stringify([selection.characterId, selection.conversationId, selection.characterIndex, selection.chatIndex])
        if (key === requested) return
        requested = key
        const token = ++request
        const { characterId, conversationId } = selection
        if (characterId === null) {
            current = unselected
            onChange()
            return
        }
        current = null
        void dependencies.resolvePosition(characterId, conversationId).catch((error) => {
            console.error(error)
            return { characterIndex: -1, chatIndex: -1 }
        }).then((position) => {
            if (token !== request) return
            current = {
                characterId,
                conversationId,
                characterIndex: position.characterIndex,
                chatIndex: conversationId === null ? -1 : position.chatIndex,
            }
            onChange()
        })
    }

    return {
        readConversation: () => current,
        watchConversation(onChange) {
            const stop = dependencies.watchSelection(() => refresh(onChange))
            refresh(onChange)
            return () => {
                stop()
                current = null
                requested = undefined
                request++
            }
        },
    }
}
