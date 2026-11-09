import type { Chat, Message } from '../storage/database.svelte'
import type { ConversationMessageMetadata } from '../storage/persistentDataStore'

const DEFAULT_PAGE_SIZE = 64

export interface HistoryWindowReader {
    readonly totalMessages: number
    /** Messages `[startIndex, startIndex + limit)`; every requested row must be present. */
    read(startIndex: number, limit: number): Promise<Message[]>
}

export interface HistoryWindowSelectionInput {
    reader: HistoryWindowReader
    tokenBudget: number
    countTokens(text: string): Promise<number>
    /** Enabled messages the window keeps even when fewer reach the token budget. */
    minimumMessages: number
    /** The window starts at or before this absolute index when it is set. */
    extendTo?: number | null
    pageSize?: number
    assertCurrent?(): void
}

export interface HistoryWindowSelection {
    start: number
    messages: Message[]
    totalMessages: number
    /** The walk ended on an `allBefore` marker, which the window includes. */
    endedAtAllBefore: boolean
    bodyRows: number
    bodyPages: number
}

export async function selectHistoryWindow(
    input: HistoryWindowSelectionInput,
): Promise<HistoryWindowSelection> {
    const total = input.reader.totalMessages
    const pageSize = input.pageSize ?? DEFAULT_PAGE_SIZE
    let loaded: Message[] = []
    let loadedStart = total
    let bodyPages = 0
    const loadBefore = async (index: number) => {
        while (loadedStart > index) {
            const pageStart = Math.max(index, loadedStart - pageSize, 0)
            const limit = loadedStart - pageStart
            const page = await input.reader.read(pageStart, limit)
            input.assertCurrent?.()
            if (page.length !== limit) throw new Error('History window page is incomplete')
            loaded = page.concat(loaded)
            loadedStart = pageStart
            bodyPages += 1
        }
    }

    let start = total
    let tokens = 0
    let enabled = 0
    let endedAtAllBefore = false
    let reachedBudget = false
    while (start > 0 && !reachedBudget && !endedAtAllBefore) {
        if (loadedStart >= start) await loadBefore(Math.max(start - pageSize, 0))
        const index = start - 1
        const message = loaded[index - loadedStart]
        start = index
        if (message.disabled === 'allBefore') {
            endedAtAllBefore = true
            break
        }
        if (message.disabled === true) continue
        enabled += 1
        tokens += await input.countTokens(typeof message.data === 'string' ? message.data : '')
        input.assertCurrent?.()
        reachedBudget = tokens >= input.tokenBudget && enabled >= input.minimumMessages
    }

    const extendTo = input.extendTo
    while (
        !endedAtAllBefore &&
        extendTo !== null &&
        extendTo !== undefined &&
        start > Math.max(extendTo, 0)
    ) {
        if (loadedStart >= start) await loadBefore(Math.max(start - pageSize, extendTo, 0))
        const index = start - 1
        start = index
        if (loaded[index - loadedStart].disabled === 'allBefore') endedAtAllBefore = true
    }

    const messages = loaded.slice(start - loadedStart)
    return {
        start,
        messages,
        totalMessages: total,
        endedAtAllBefore,
        bodyRows: loaded.length,
        bodyPages,
    }
}

/** The messages from `start` through the newest one, without a token walk. */
export async function readHistoryWindowTail(
    reader: HistoryWindowReader,
    start: number,
    assertCurrent?: () => void,
    pageSize = DEFAULT_PAGE_SIZE,
): Promise<HistoryWindowSelection> {
    const total = reader.totalMessages
    const from = Math.min(Math.max(Math.trunc(start) || 0, 0), total)
    const messages: Message[] = []
    let bodyPages = 0
    for (let index = from; index < total;) {
        const limit = Math.min(pageSize, total - index)
        const page = await reader.read(index, limit)
        assertCurrent?.()
        if (page.length !== limit) throw new Error('History window page is incomplete')
        messages.push(...page)
        index += limit
        bodyPages += 1
    }
    return {
        start: from,
        messages,
        totalMessages: total,
        endedAtAllBefore: false,
        bodyRows: messages.length,
        bodyPages,
    }
}

export interface HistoryWindowHypaPlan {
    /** Ids of the enabled messages after the last `allBefore` marker. */
    effectiveMessageMemos: string[]
    /**
     * Absolute index the window must reach so HypaV3 sees every unsummarized
     * message: the newest summary's last message, the last `allBefore` marker
     * (or 0) when nothing is summarized, or null when that message no longer
     * exists and the token window stands alone.
     */
    extendTo: number | null
}

function summaryMemos(summary: unknown): string[] | null {
    if (!summary || typeof summary !== 'object' || !('chatMemos' in summary)) return null
    const memos = (summary as { chatMemos: unknown }).chatMemos
    const values = memos instanceof Set ? [...memos] : memos
    return Array.isArray(values) && values.every((memo) => typeof memo === 'string')
        ? values as string[]
        : null
}

/** Mirrors the start index and orphan cleaning HypaV3 applies to the full message list. */
export function planHistoryWindowHypaV3(
    conversation: Omit<Chat, 'message'>,
    messages: readonly Pick<ConversationMessageMetadata, 'chatId' | 'disabled'>[],
    preserveOrphanedMemory: boolean,
): HistoryWindowHypaPlan {
    let markerIndex = -1
    for (let index = messages.length - 1; index >= 0; index -= 1) {
        if (messages[index].disabled === 'allBefore') {
            markerIndex = index
            break
        }
    }
    const effectiveMessageMemos: string[] = []
    const positions = new Map<string, number>()
    for (let index = markerIndex + 1; index < messages.length; index += 1) {
        const message = messages[index]
        if (message.disabled === true || message.disabled === 'allBefore') continue
        if (typeof message.chatId !== 'string' || message.chatId.length === 0) continue
        effectiveMessageMemos.push(message.chatId)
        if (!positions.has(message.chatId)) positions.set(message.chatId, index)
    }
    const unsummarized = { effectiveMessageMemos, extendTo: Math.max(markerIndex, 0) }

    const raw = (conversation.hypaV3Data as { summaries?: unknown } | undefined)?.summaries
    const summaries = Array.isArray(raw) ? raw.map(summaryMemos) : []
    if (summaries.length === 0 || summaries.some((memos) => memos === null)) return unsummarized
    const memoSet = new Set(effectiveMessageMemos)
    const surviving = preserveOrphanedMemory
        ? summaries as string[][]
        : (summaries as string[][]).filter((memos) => memos.every((memo) => memoSet.has(memo)))
    const boundaryMemo = surviving.at(-1)?.at(-1)
    if (boundaryMemo === undefined) return unsummarized
    return { effectiveMessageMemos, extendTo: positions.get(boundaryMemo) ?? null }
}
