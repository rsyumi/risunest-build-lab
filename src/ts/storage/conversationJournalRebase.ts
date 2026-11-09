import type { PersistentRevisionLease } from './persistentDataStore'

export interface ConversationHistorySpan {
    originalStart: number
    currentStart: number
    count: number
}

export type ConversationHistoryMapping = {
    kind: 'mapped'
    originalCount: number
    currentCount: number
    spans: ConversationHistorySpan[]
} | { kind: 'conflict'; reason: 'ambiguous-history' | 'history-budget' | 'missing-history' }

export interface ConversationJournalRange {
    start: number
    deleteCount: number
    insertCount: number
}

export class ConversationJournalConflictError extends Error {
    constructor(readonly reason: string) {
        super(`Conversation edits require reconciliation: ${reason}`)
        this.name = 'ConversationJournalConflictError'
    }
}

type Piece = { origin: number; start: number; count: number }
const length = (pieces: readonly Piece[]) => pieces.reduce((sum, piece) => sum + piece.count, 0)
const conflict = (reason: string): never => { throw new ConversationJournalConflictError(reason) }

function slice(pieces: readonly Piece[], start: number, count: number): Piece[] {
    const result: Piece[] = []
    let offset = 0
    for (const piece of pieces) {
        const from = Math.max(0, start - offset)
        const to = Math.min(piece.count, start + count - offset)
        if (to > from) result.push({ origin: piece.origin, start: piece.start + from, count: to - from })
        offset += piece.count
        if (offset >= start + count) break
    }
    return result
}

function replace(pieces: Piece[], start: number, count: number, inserted: Piece[]): Piece[] {
    return [...slice(pieces, 0, start), ...inserted, ...slice(pieces, start + count, length(pieces) - start - count)]
}

function position(pieces: readonly Piece[], token: Piece): number | null {
    let offset = 0
    for (const piece of pieces) {
        if (piece.origin === token.origin && token.start >= piece.start && token.start < piece.start + piece.count) {
            return offset + token.start - piece.start
        }
        offset += piece.count
    }
    return null
}

function sameOrigins(left: readonly Piece[], right: readonly Piece[]): boolean {
    let a = 0, b = 0, ai = 0, bi = 0
    while (a < left.length && b < right.length) {
        if (left[a].origin !== right[b].origin || left[a].start + ai !== right[b].start + bi) return false
        const count = Math.min(left[a].count - ai, right[b].count - bi)
        ai += count; bi += count
        if (ai === left[a].count) { a++; ai = 0 }
        if (bi === right[b].count) { b++; bi = 0 }
    }
    return a === left.length && b === right.length
}

/** Translates ordered local edits, including edits to locally inserted rows. */
export function rebaseConversationJournal(
    mapping: ConversationHistoryMapping,
    ranges: readonly ConversationJournalRange[],
    maxPieces = 4096,
): ConversationJournalRange[] {
    if (mapping.kind === 'conflict') return conflict(mapping.reason)
    if (![mapping.originalCount, mapping.currentCount, maxPieces].every(value => Number.isSafeInteger(value) && value >= 0)) return conflict('invalid-history-proof')
    let original: Piece[] = mapping.originalCount ? [{ origin: 0, start: 0, count: mapping.originalCount }] : []
    let current: Piece[] = []
    let originalEnd = 0, currentEnd = 0
    for (const span of mapping.spans) {
        if (!Number.isSafeInteger(span.originalStart) || !Number.isSafeInteger(span.currentStart) ||
            !Number.isSafeInteger(span.count) || span.count <= 0 || span.originalStart < originalEnd ||
            span.currentStart < currentEnd || span.originalStart + span.count > mapping.originalCount ||
            span.currentStart + span.count > mapping.currentCount) return conflict('invalid-history-proof')
        if (span.currentStart > currentEnd) current.push({ origin: -1, start: currentEnd, count: span.currentStart - currentEnd })
        current.push({ origin: 0, start: span.originalStart, count: span.count })
        originalEnd = span.originalStart + span.count
        currentEnd = span.currentStart + span.count
    }
    if (currentEnd < mapping.currentCount) current.push({ origin: -1, start: currentEnd, count: mapping.currentCount - currentEnd })
    if (original.length + current.length > maxPieces) return conflict('journal-budget')
    const rebased: ConversationJournalRange[] = []
    for (const [index, range] of ranges.entries()) {
        if (![range.start, range.deleteCount, range.insertCount].every(value => Number.isSafeInteger(value) && value >= 0) ||
            range.start + range.deleteCount > length(original)) return conflict('invalid-journal-range')
        let start: number
        if (range.deleteCount) {
            const removed = slice(original, range.start, range.deleteCount)
            const mapped = position(current, removed[0])
            if (mapped === null || !sameOrigins(removed, slice(current, mapped, range.deleteCount))) return conflict('overlapping-history')
            start = mapped
        } else {
            const before = range.start ? slice(original, range.start - 1, 1)[0] : null
            const after = range.start < length(original) ? slice(original, range.start, 1)[0] : null
            const left = before ? position(current, before) : -1
            const right = after ? position(current, after) : length(current)
            if (left === null || right === null || left + 1 !== right) return conflict('changed-insertion-gap')
            start = right
        }
        const inserted = range.insertCount ? [{ origin: index + 1, start: 0, count: range.insertCount }] : []
        original = replace(original, range.start, range.deleteCount, inserted)
        current = replace(current, start, range.deleteCount, inserted)
        if (original.length + current.length > maxPieces) return conflict('journal-budget')
        rebased.push({ ...range, start })
    }
    return rebased
}

export interface ConversationHistoryRebaseRequest {
    baseline: PersistentRevisionLease
    current: PersistentRevisionLease
    characterId: string
    conversationId: string
}

