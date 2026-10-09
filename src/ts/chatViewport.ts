export type ChatViewportPinReason = 'streaming' | 'editor' | 'playing-media' | 'latest-pair'

export interface ChatViewportAnchor {
    key: string
    indexHint: number
    relativeOffset: number
}

export interface ChatViewportPin {
    key: string
    reason: ChatViewportPinReason
    indexHint?: number
}

export interface ChatViewportJumpOptions {
    align?: 'start' | 'center' | 'end'
    // Height covered by an overlay at the bottom of the scroll area; 'end' keeps the row above it.
    bottomInset?: number
    highlight?: boolean
}

export type ChatViewportStepDirection = 'previous' | 'next'

export interface ChatViewportRowBounds {
    index: number
    top: number
    bottom: number
}

export type ChatViewportStepTarget =
    | { kind: 'row'; index: number; align: 'start' | 'end' }
    | { kind: 'top' }
    | { kind: 'bottom' }

const STEP_EDGE_TOLERANCE = 8

// Previous aligns the start of the row crossing the top edge, then of the row above it.
// Next aligns the end of the row crossing the bottom edge, then of the row below it.
export function resolveChatViewportStep(
    rows: readonly ChatViewportRowBounds[],
    visibleTop: number,
    visibleBottom: number,
    rowCount: number,
    direction: ChatViewportStepDirection,
): ChatViewportStepTarget {
    const sorted = rows
        .filter((row) => row.bottom > row.top)
        .sort((left, right) => left.index - right.index)
    if (direction === 'previous') {
        const current =
            sorted.find((row) => row.bottom > visibleTop + STEP_EDGE_TOLERANCE) ?? sorted.at(-1)
        if (!current) return { kind: 'top' }
        if (current.top < visibleTop - STEP_EDGE_TOLERANCE)
            return { kind: 'row', index: current.index, align: 'start' }
        return current.index > 0
            ? { kind: 'row', index: current.index - 1, align: 'start' }
            : { kind: 'top' }
    }
    let current = sorted[0]
    for (const row of sorted) {
        if (row.top < visibleBottom - STEP_EDGE_TOLERANCE) current = row
    }
    if (!current) return { kind: 'bottom' }
    if (current.bottom > visibleBottom + STEP_EDGE_TOLERANCE)
        return { kind: 'row', index: current.index, align: 'end' }
    return current.index < rowCount - 1
        ? { kind: 'row', index: current.index + 1, align: 'end' }
        : { kind: 'bottom' }
}

export interface ChatViewportHandle {
    isAtBottom(): boolean
    jumpTo(index: number, options?: ChatViewportJumpOptions): Promise<boolean>
    jumpToLatestMessage(): Promise<void>
    scrollToLatestMessage(): Promise<void>
    jumpToTop(): Promise<boolean>
    jumpToBottom(): Promise<boolean>
    navigateMessage(direction: ChatViewportStepDirection, bottomInset?: number): Promise<boolean>
}

export interface ChatViewportKeySource {
    readonly length: number
    keyAt(index: number): string | undefined
    indexOf(key: string): number
}

export interface ChatViewportInput {
    keys?: readonly string[]
    keySource?: ChatViewportKeySource
    budget: number
    overscan: number
    estimatedMessageHeight: number
    measuredHeights?: ReadonlyMap<string, number>
    measuredHeightsByIndex?: ReadonlyMap<number, number>
    anchor?: ChatViewportAnchor | null
    jumpTarget?: number
    pins?: readonly ChatViewportPin[]
    viewportHeight?: number
    preparationScreens?: number
    retentionScreens?: number
    retainedIndices?: readonly number[]
}

export interface ChatViewportMessageRow {
    kind: 'message'
    index: number
    key: string
    pinReasons: readonly ChatViewportPinReason[]
}

export interface ChatViewportGapRow {
    kind: 'gap'
    startIndex: number
    endIndex: number
    height: number
}

export type ChatViewportRow = ChatViewportMessageRow | ChatViewportGapRow

export interface ChatViewportPinOverflow {
    count: number
    pinnedMessageCount: number
    pinnedKeys: readonly string[]
}

export interface ChatViewportResult {
    rows: readonly ChatViewportRow[]
    messageRows: readonly ChatViewportMessageRow[]
    mountedMessageCount: number
    anchor: ChatViewportAnchor | null
    jumpAccepted: boolean | null
    pinOverflow: ChatViewportPinOverflow | null
}

function normalizeBudget(value: number): number {
    return Number.isInteger(value) && value > 0 ? value : 1
}

function normalizeOverscan(value: number, budget: number): number {
    if (!Number.isInteger(value) || value < 0) return 0
    return Math.min(value, budget - 1)
}

function keySource(input: ChatViewportInput): ChatViewportKeySource {
    if (input.keySource) return input.keySource
    const keys = input.keys ?? []
    return {
        length: keys.length,
        keyAt: (index) => keys[index],
        indexOf: (key) => keys.indexOf(key),
    }
}

function requireKey(source: ChatViewportKeySource, index: number): string {
    const key = source.keyAt(index)
    if (key === undefined) throw new RangeError(`Viewport key ${index} is unavailable`)
    return key
}

function resolveAnchor(
    source: ChatViewportKeySource,
    anchor: ChatViewportAnchor | null | undefined,
): ChatViewportAnchor | null {
    if (source.length === 0) return null
    if (!anchor) {
        const index = source.length - 1
        return { key: requireKey(source, index), indexHint: index, relativeOffset: 0 }
    }

    const hintedIndex = Math.trunc(anchor.indexHint)
    const stableIndex = (
        hintedIndex >= 0 &&
        hintedIndex < source.length &&
        source.keyAt(hintedIndex) === anchor.key
    )
        ? hintedIndex
        : source.indexOf(anchor.key)
    if (stableIndex >= 0) return { ...anchor, indexHint: stableIndex }

    const index = Math.min(Math.max(Math.trunc(anchor.indexHint), 0), source.length - 1)
    return { key: requireKey(source, index), indexHint: index, relativeOffset: anchor.relativeOffset }
}

export function measureChatViewportRange(
    startIndex: number,
    endIndex: number,
    estimatedHeight: number,
    measurements: ReadonlyMap<number, number>,
    reserveZeroHeight = false,
): number {
    const estimate = Number.isFinite(estimatedHeight) ? Math.max(0, estimatedHeight) : 0
    let height = (endIndex - startIndex) * estimate
    for (const [index, measured] of measurements) {
        if (index >= startIndex && index < endIndex && Number.isFinite(measured) && measured >= 0 && (!reserveZeroHeight || measured > 0)) {
            height += measured - estimate
        }
    }
    return height
}

export function locateChatViewportOffset(
    startIndex: number,
    endIndex: number,
    distance: number,
    movingOlder: boolean,
    estimatedHeight: number,
    measurements: ReadonlyMap<number, number>,
): { index: number; traversed: number; height: number } {
    let low = 0
    let high = Math.max(0, endIndex - startIndex - 1)
    const heightThrough = (offset: number) => movingOlder
        ? measureChatViewportRange(endIndex - offset - 1, endIndex, estimatedHeight, measurements)
        : measureChatViewportRange(startIndex, startIndex + offset + 1, estimatedHeight, measurements)
    while (low < high) {
        const middle = Math.floor((low + high) / 2)
        if (heightThrough(middle) >= distance) high = middle
        else low = middle + 1
    }
    const index = movingOlder ? endIndex - low - 1 : startIndex + low
    const height = measureChatViewportRange(index, index + 1, estimatedHeight, measurements)
    return { index, traversed: heightThrough(low) - height, height }
}

function gapHeight(
    input: ChatViewportInput,
    source: ChatViewportKeySource,
    startIndex: number,
    endIndex: number,
    reserveZeroHeight = false,
): number {
    const estimate = Number.isFinite(input.estimatedMessageHeight)
        ? Math.max(0, input.estimatedMessageHeight)
        : 0
    let height = (endIndex - startIndex) * estimate
    if (input.measuredHeightsByIndex) {
        return measureChatViewportRange(startIndex, endIndex, estimate, input.measuredHeightsByIndex, reserveZeroHeight)
    }
    if (!input.measuredHeights) return height
    for (let index = startIndex; index < endIndex; index++) {
        const measured = input.measuredHeights.get(requireKey(source, index))
        if (measured !== undefined && Number.isFinite(measured) && measured >= 0 && (!reserveZeroHeight || measured > 0)) {
            height += measured - estimate
        }
    }
    return height
}

function createRows(
    input: ChatViewportInput,
    source: ChatViewportKeySource,
    messageRows: readonly ChatViewportMessageRow[],
): ChatViewportRow[] {
    const rows: ChatViewportRow[] = []
    let nextIndex = 0
    for (const messageRow of messageRows) {
        if (nextIndex < messageRow.index) {
            rows.push({
                kind: 'gap',
                startIndex: nextIndex,
                endIndex: messageRow.index,
                height: gapHeight(input, source, nextIndex, messageRow.index),
            })
        }
        rows.push(messageRow)
        nextIndex = messageRow.index + 1
    }
    if (nextIndex < source.length) {
        rows.push({
            kind: 'gap',
            startIndex: nextIndex,
            endIndex: source.length,
            height: gapHeight(input, source, nextIndex, source.length),
        })
    }
    return rows
}

export function buildChatViewport(input: ChatViewportInput): ChatViewportResult {
    const source = keySource(input)
    const budget = normalizeBudget(input.budget)
    const overscan = normalizeOverscan(input.overscan, budget)
    const previousAnchor = resolveAnchor(source, input.anchor)
    const hasJump = input.jumpTarget !== undefined
    const jumpAccepted = hasJump
        ? Number.isInteger(input.jumpTarget) && input.jumpTarget! >= 0 && input.jumpTarget! < source.length
        : null
    const anchor = jumpAccepted
        ? {
            key: requireKey(source, input.jumpTarget!),
            indexHint: input.jumpTarget!,
            relativeOffset: 0,
        }
        : previousAnchor
    const focusIndex = anchor?.indexHint ?? 0
    const beforeFocus = overscan
    const maxStart = Math.max(0, source.length - budget)
    const startIndex = input.anchor || hasJump
        ? Math.min(Math.max(0, focusIndex - beforeFocus), maxStart)
        : maxStart
    const endIndex = Math.min(source.length, startIndex + budget)
    const selectedIndices = new Set<number>()
    const requiredIndices = new Set<number>()
    const viewportHeight = input.viewportHeight ?? 0
    if (viewportHeight > 0 && Number.isFinite(viewportHeight) && anchor) {
        const preparation = Math.max(0, input.preparationScreens ?? 1) * viewportHeight
        const retention = Math.max(input.preparationScreens ?? 1, input.retentionScreens ?? 3) * viewportHeight
        const rowHeight = (index: number) => {
            const measured = input.measuredHeightsByIndex?.get(index)
                ?? input.measuredHeights?.get(requireKey(source, index))
            return measured !== undefined && Number.isFinite(measured) && measured > 0
                ? measured : Math.max(1, input.estimatedMessageHeight)
        }
        // A new conversation follows the bottom; navigation uses the row's top offset.
        const offset = !input.anchor && !hasJump
            ? viewportHeight - rowHeight(focusIndex) : anchor.relativeOffset
        selectedIndices.add(focusIndex)
        requiredIndices.add(focusIndex)
        let top = offset
        for (let index = focusIndex - 1; index >= 0 && top > -preparation; index--) {
            const bottom = top
            top -= rowHeight(index)
            const visible = bottom > 0 && top < viewportHeight
            if (visible) requiredIndices.add(index)
            if (visible || selectedIndices.size < budget) selectedIndices.add(index)
            else break
        }
        let bottom = offset + rowHeight(focusIndex)
        for (let index = focusIndex + 1; index < source.length && bottom < viewportHeight + preparation; index++) {
            const top = bottom
            bottom += rowHeight(index)
            const visible = bottom > 0 && top < viewportHeight
            if (visible) requiredIndices.add(index)
            if (visible || selectedIndices.size < budget) selectedIndices.add(index)
            else break
        }
        for (const index of input.retainedIndices ?? []) {
            if (!Number.isInteger(index) || index < 0 || index >= source.length) continue
            const top = index < focusIndex
                ? offset - gapHeight(input, source, index, focusIndex, true)
                : offset + gapHeight(input, source, focusIndex, index, true)
            if (top + rowHeight(index) > -retention && top < viewportHeight + retention) {
                selectedIndices.add(index)
            }
        }
    } else {
        for (let index = startIndex; index < endIndex; index++) selectedIndices.add(index)
        if (anchor) requiredIndices.add(focusIndex)
    }

    const pinReasonsByIndex = new Map<number, ChatViewportPinReason[]>()
    for (const pin of input.pins ?? []) {
        const hintedIndex = pin.indexHint
        const index = (
            hintedIndex !== undefined &&
            Number.isSafeInteger(hintedIndex) &&
            hintedIndex >= 0 &&
            hintedIndex < source.length &&
            source.keyAt(hintedIndex) === pin.key
        )
            ? hintedIndex
            : source.indexOf(pin.key)
        if (index < 0) continue
        const reasons = pinReasonsByIndex.get(index) ?? []
        if (!reasons.includes(pin.reason)) reasons.push(pin.reason)
        pinReasonsByIndex.set(index, reasons)
        selectedIndices.add(index)
    }

    const protectedIndices = new Set([...requiredIndices, ...pinReasonsByIndex.keys()])
    const mountedLimit = viewportHeight > 0 ? selectedIndices.size : Math.max(budget, protectedIndices.size)
    while (selectedIndices.size > mountedLimit) {
        const removable = [...selectedIndices]
            .filter((index) => !protectedIndices.has(index))
            .sort((left, right) => {
                const distance = Math.abs(right - focusIndex) - Math.abs(left - focusIndex)
                return distance || right - left
            })[0]
        if (removable === undefined) break
        selectedIndices.delete(removable)
    }

    const messageRows = [...selectedIndices]
        .sort((left, right) => left - right)
        .map((index): ChatViewportMessageRow => ({
            kind: 'message',
            index,
            key: requireKey(source, index),
            pinReasons: pinReasonsByIndex.get(index) ?? [],
        }))
    const rows = createRows(input, source, messageRows)

    return {
        rows,
        messageRows,
        mountedMessageCount: messageRows.length,
        anchor,
        jumpAccepted,
        pinOverflow: pinReasonsByIndex.size > budget
            ? {
                count: pinReasonsByIndex.size - budget,
                pinnedMessageCount: pinReasonsByIndex.size,
                pinnedKeys: [...pinReasonsByIndex.keys()]
                    .sort((left, right) => left - right)
                    .map((index) => requireKey(source, index)),
            }
            : null,
    }
}
