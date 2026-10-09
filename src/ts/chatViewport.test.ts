import { describe, expect, it } from 'vitest'
import { buildChatViewport, locateChatViewportOffset, resolveChatViewportStep } from './chatViewport'

function keys(count: number): string[] {
    return Array.from({ length: count }, (_, index) => `message-${index}`)
}

describe('buildChatViewport', () => {
    it('keeps one tall reading row plus required tail rows without filling a count', () => {
        const viewport = buildChatViewport({
            keys: keys(200), budget: 64, overscan: 8, estimatedMessageHeight: 256,
            viewportHeight: 600,
            anchor: { key: 'message-50', indexHint: 50, relativeOffset: -4_000 },
            measuredHeightsByIndex: new Map([[50, 10_000]]),
            pins: [
                { key: 'message-198', reason: 'latest-pair' },
                { key: 'message-199', reason: 'latest-pair' },
            ],
        })
        expect(viewport.messageRows.map(row => row.index)).toEqual([50, 198, 199])
    })

    it('covers short rows despite distant pins and retains only existing nearby DOM', () => {
        const base = {
            keys: keys(100), budget: 2, overscan: 0, estimatedMessageHeight: 100,
            viewportHeight: 500,
            anchor: { key: 'message-20', indexHint: 20, relativeOffset: 0 },
            pins: [{ key: 'message-99', reason: 'latest-pair' as const }],
        }
        const viewport = buildChatViewport({ ...base, retainedIndices: [4, 15, 30, 70] })
        const indices = viewport.messageRows.map(row => row.index)
        expect(indices).toEqual(expect.arrayContaining([20, 21, 22, 23, 24, 99, 15, 30]))
        expect(indices).not.toContain(4)
        expect(indices).not.toContain(70)
        expect(indices).not.toContain(14)
    })

    it('bounds initial zero-height rows and uses bottom geometry on entry', () => {
        const viewport = buildChatViewport({
            keys: keys(200), budget: 64, overscan: 8, estimatedMessageHeight: 256,
            viewportHeight: 600,
            measuredHeightsByIndex: new Map([[199, 10_000], [198, 0]]),
        })
        expect(viewport.messageRows.map(row => row.index)).toEqual([199])
        const unknown = buildChatViewport({
            keys: keys(200), budget: 64, overscan: 8, estimatedMessageHeight: 256,
            viewportHeight: 600,
            measuredHeightsByIndex: new Map(Array.from({ length: 200 }, (_, i) => [i, 0])),
        })
        expect(unknown.mountedMessageCount).toBe(5)
        const retained = buildChatViewport({
            keys: keys(200), budget: 64, overscan: 8, estimatedMessageHeight: 256,
            viewportHeight: 600,
            measuredHeightsByIndex: new Map(Array.from({ length: 200 }, (_, i) => [i, 0])),
            retainedIndices: Array.from({ length: 200 }, (_, i) => i),
        })
        expect(retained.mountedMessageCount).toBeLessThan(12)
    })

    it('uses a bounded key source and indexed height corrections without scanning omitted rows', () => {
        let keyReads = 0
        let indexLookups = 0
        const viewport = buildChatViewport({
            keySource: {
                length: 10_000,
                keyAt(index) {
                    keyReads += 1
                    return `message-${index}`
                },
                indexOf(key) {
                    indexLookups += 1
                    return Number(key.slice('message-'.length))
                },
            },
            budget: 64,
            overscan: 8,
            estimatedMessageHeight: 100,
            measuredHeightsByIndex: new Map([
                [0, 150],
                [5_000, 80],
            ]),
            anchor: { key: 'message-5000', indexHint: 5_000, relativeOffset: 17 },
            pins: [{ key: 'message-5000', reason: 'editor', indexHint: 5_000 }],
        })

        expect(viewport.messageRows).toHaveLength(64)
        expect(viewport.messageRows.some((row) => row.index === 5_000)).toBe(true)
        expect(viewport.rows[0]).toEqual({
            kind: 'gap',
            startIndex: 0,
            endIndex: 4_992,
            height: 499_250,
        })
        expect(keyReads).toBeLessThanOrEqual(66)
        expect(indexLookups).toBe(0)
    })

    it('starts at the newest tail and counts overscan inside the mounted budget', () => {
        const normal = buildChatViewport({
            keys: keys(100),
            budget: 64,
            overscan: 8,
            estimatedMessageHeight: 100,
        })
        const lowSpec = buildChatViewport({
            keys: keys(100),
            budget: 40,
            overscan: 8,
            estimatedMessageHeight: 100,
        })

        expect(normal.messageRows.map((row) => row.index)).toEqual(
            Array.from({ length: 64 }, (_, index) => index + 36),
        )
        expect(normal.mountedMessageCount).toBe(64)
        expect(lowSpec.messageRows.map((row) => row.index)).toEqual(
            Array.from({ length: 40 }, (_, index) => index + 60),
        )
        expect(lowSpec.mountedMessageCount).toBe(40)
    })

    it('jumps directly within 10,000 messages and rejects out-of-range targets without moving the anchor', () => {
        const messageKeys = keys(10_000)
        const anchor = { key: 'message-9000', indexHint: 9000, relativeOffset: 17 }
        const jumped = buildChatViewport({
            keys: messageKeys,
            budget: 64,
            overscan: 8,
            estimatedMessageHeight: 100,
            anchor,
            jumpTarget: 100,
        })

        expect(jumped.jumpAccepted).toBe(true)
        expect(jumped.messageRows.some((row) => row.index === 100)).toBe(true)
        expect(jumped.messageRows).toHaveLength(64)
        expect(jumped.messageRows[0].index).toBe(92)
        expect(jumped.messageRows.some((row) => row.index === 9000)).toBe(false)

        const rejected = buildChatViewport({
            keys: messageKeys,
            budget: 64,
            overscan: 8,
            estimatedMessageHeight: 100,
            anchor,
            jumpTarget: 10_000,
        })

        expect(rejected.jumpAccepted).toBe(false)
        expect(rejected.anchor).toEqual(anchor)
        expect(rejected.messageRows.some((row) => row.index === 9000)).toBe(true)
    })

    it('returns measured gap rows while preserving the stable anchor and relative offset across height updates', () => {
        const anchor = { key: 'message-5', indexHint: 5, relativeOffset: 23 }
        const before = buildChatViewport({
            keys: keys(10),
            budget: 3,
            overscan: 1,
            estimatedMessageHeight: 100,
            measuredHeights: new Map([['message-1', 150]]),
            anchor,
        })
        const after = buildChatViewport({
            keys: keys(10),
            budget: 3,
            overscan: 1,
            estimatedMessageHeight: 100,
            measuredHeights: new Map([['message-1', 250]]),
            anchor: before.anchor,
        })

        expect(before.rows).toEqual([
            { kind: 'gap', startIndex: 0, endIndex: 4, height: 450 },
            { kind: 'message', index: 4, key: 'message-4', pinReasons: [] },
            { kind: 'message', index: 5, key: 'message-5', pinReasons: [] },
            { kind: 'message', index: 6, key: 'message-6', pinReasons: [] },
            { kind: 'gap', startIndex: 7, endIndex: 10, height: 300 },
        ])
        expect(after.rows[0]).toEqual({ kind: 'gap', startIndex: 0, endIndex: 4, height: 550 })
        expect(after.anchor).toEqual(anchor)
    })

    it('keeps disjoint streaming, editor, and playing-media pins inside the budget with independent reasons', () => {
        const viewport = buildChatViewport({
            keys: keys(20),
            budget: 6,
            overscan: 1,
            estimatedMessageHeight: 10,
            anchor: { key: 'message-10', indexHint: 10, relativeOffset: 0 },
            pins: [
                { key: 'message-1', reason: 'editor' },
                { key: 'message-18', reason: 'playing-media' },
                { key: 'message-18', reason: 'editor' },
                { key: 'message-19', reason: 'streaming' },
            ],
        })

        expect(viewport.messageRows.map((row) => row.index)).toEqual([1, 9, 10, 11, 18, 19])
        expect(viewport.messageRows.find((row) => row.index === 18)?.pinReasons).toEqual([
            'playing-media',
            'editor',
        ])
        expect(viewport.rows.filter((row) => row.kind === 'gap')).toHaveLength(3)
        expect(viewport.mountedMessageCount).toBe(6)
        expect(viewport.pinOverflow).toBeNull()
    })

    it('keeps every active pin, including the newest streaming row, and reports explicit overflow', () => {
        const viewport = buildChatViewport({
            keys: keys(10),
            budget: 3,
            overscan: 1,
            estimatedMessageHeight: 10,
            anchor: { key: 'message-5', indexHint: 5, relativeOffset: 0 },
            pins: [
                { key: 'message-0', reason: 'editor' },
                { key: 'message-2', reason: 'playing-media' },
                { key: 'message-7', reason: 'playing-media' },
                { key: 'message-9', reason: 'streaming' },
            ],
        })

        expect(viewport.messageRows.map((row) => row.index)).toEqual([0, 2, 5, 7, 9])
        expect(viewport.messageRows.at(-1)).toMatchObject({ index: 9, pinReasons: ['streaming'] })
        expect(viewport.mountedMessageCount).toBe(5)
        expect(viewport.pinOverflow).toEqual({
            count: 1,
            pinnedMessageCount: 4,
            pinnedKeys: ['message-0', 'message-2', 'message-7', 'message-9'],
        })
    })

    it('tracks a logical anchor through inserts and rerolls, then falls back deterministically after deletion', () => {
        const anchor = { key: 'c', indexHint: 2, relativeOffset: 11 }
        const inserted = buildChatViewport({
            keys: ['a', 'x', 'b', 'c', 'd', 'e'],
            budget: 3,
            overscan: 1,
            estimatedMessageHeight: 10,
            anchor,
        })
        const rerolled = buildChatViewport({
            keys: ['a', 'x', 'b', 'c', 'd', 'e'],
            budget: 3,
            overscan: 1,
            estimatedMessageHeight: 10,
            anchor: inserted.anchor,
        })
        const deleted = buildChatViewport({
            keys: ['a', 'x', 'b', 'd', 'e'],
            budget: 3,
            overscan: 1,
            estimatedMessageHeight: 10,
            anchor: rerolled.anchor,
        })

        expect(inserted.anchor).toEqual({ key: 'c', indexHint: 3, relativeOffset: 11 })
        expect(rerolled.anchor).toEqual(inserted.anchor)
        expect(deleted.anchor).toEqual({ key: 'd', indexHint: 3, relativeOffset: 11 })
    })
})

describe('locateChatViewportOffset', () => {
    it.each([false, true])('matches a linear walk including zero-height rows (older=%s)', (older) => {
        const measured = new Map([[4, 0], [5, 600], [11, 40]])
        for (let distance = 0; distance < 2_400; distance += 20) {
            let index = older ? 19 : 0
            let traversed = 0
            let height = measured.get(index) ?? 100
            while (traversed + height < distance && (older ? index > 0 : index < 19)) {
                traversed += height
                index += older ? -1 : 1
                height = measured.get(index) ?? 100
            }
            expect(locateChatViewportOffset(0, 20, distance, older, 100, measured))
                .toEqual({ index, traversed, height })
        }
    })
    it('finds a far offset without walking every omitted message', () => {
        expect(locateChatViewportOffset(0, 1_000_000, 90_000_050, false, 100, new Map()))
            .toEqual({ index: 900_000, traversed: 90_000_000, height: 100 })
    })
})

describe('resolveChatViewportStep', () => {
    // Rows 3..6 with the visible area spanning 100..500.
    const rows = [
        { index: 3, top: -250, bottom: 150 },
        { index: 4, top: 160, bottom: 300 },
        { index: 5, top: 310, bottom: 640 },
        { index: 6, top: 650, bottom: 900 },
    ]

    it('aligns the start of the row crossing the top edge, then the start of the row above', () => {
        expect(resolveChatViewportStep(rows, 100, 500, 10, 'previous')).toEqual({ kind: 'row', index: 3, align: 'start' })
        const aligned = rows.map((row) => ({ ...row, top: row.top + 350, bottom: row.bottom + 350 }))
        expect(resolveChatViewportStep(aligned, 100, 500, 10, 'previous')).toEqual({ kind: 'row', index: 2, align: 'start' })
    })

    it('aligns the end of the row crossing the bottom edge, then the end of the row below', () => {
        expect(resolveChatViewportStep(rows, 100, 500, 10, 'next')).toEqual({ kind: 'row', index: 5, align: 'end' })
        const aligned = rows.map((row) => ({ ...row, top: row.top - 140, bottom: row.bottom - 140 }))
        expect(resolveChatViewportStep(aligned, 100, 500, 10, 'next')).toEqual({ kind: 'row', index: 6, align: 'end' })
    })

    it('moves down past rows that are already fully visible', () => {
        const short = [
            { index: 0, top: 120, bottom: 180 },
            { index: 1, top: 190, bottom: 260 },
            { index: 2, top: 270, bottom: 340 },
        ]
        expect(resolveChatViewportStep(short, 100, 500, 5, 'next')).toEqual({ kind: 'row', index: 3, align: 'end' })
    })

    it('treats an edge within the tolerance as reached', () => {
        const nearlyAligned = [{ index: 4, top: 94, bottom: 300 }, { index: 5, top: 310, bottom: 506 }]
        expect(resolveChatViewportStep(nearlyAligned, 100, 500, 10, 'previous')).toEqual({ kind: 'row', index: 3, align: 'start' })
        expect(resolveChatViewportStep(nearlyAligned, 100, 500, 10, 'next')).toEqual({ kind: 'row', index: 6, align: 'end' })
        const partlyHidden = [{ index: 4, top: 70, bottom: 300 }, { index: 5, top: 310, bottom: 530 }]
        expect(resolveChatViewportStep(partlyHidden, 100, 500, 10, 'previous')).toEqual({ kind: 'row', index: 4, align: 'start' })
        expect(resolveChatViewportStep(partlyHidden, 100, 500, 10, 'next')).toEqual({ kind: 'row', index: 5, align: 'end' })
    })

    it('goes to the conversation edges past the first and last rows', () => {
        const whole = [{ index: 0, top: 100, bottom: 250 }, { index: 1, top: 260, bottom: 480 }]
        expect(resolveChatViewportStep(whole, 100, 500, 2, 'previous')).toEqual({ kind: 'top' })
        expect(resolveChatViewportStep(whole, 100, 500, 2, 'next')).toEqual({ kind: 'bottom' })
        expect(resolveChatViewportStep([], 100, 500, 2, 'previous')).toEqual({ kind: 'top' })
        expect(resolveChatViewportStep([], 100, 500, 2, 'next')).toEqual({ kind: 'bottom' })
    })

    it('steps from the nearest mounted row when the edge falls in an unmounted gap', () => {
        const below = [{ index: 40, top: 600, bottom: 700 }]
        expect(resolveChatViewportStep(below, 100, 500, 80, 'previous')).toEqual({ kind: 'row', index: 39, align: 'start' })
        const above = [{ index: 10, top: -300, bottom: -200 }]
        expect(resolveChatViewportStep(above, 100, 500, 80, 'next')).toEqual({ kind: 'row', index: 11, align: 'end' })
    })

    it('ignores rows without height and accepts rows in any order', () => {
        const unordered = [rows[2], { index: 9, top: 0, bottom: 0 }, rows[0], rows[3], rows[1]]
        expect(resolveChatViewportStep(unordered, 100, 500, 10, 'next')).toEqual({ kind: 'row', index: 5, align: 'end' })
        expect(resolveChatViewportStep(unordered, 100, 500, 10, 'previous')).toEqual({ kind: 'row', index: 3, align: 'start' })
    })
})
