/**
 * Splits large plain text into bounded layout units that are mounted over
 * several frames. One very long paragraph lays out superlinearly in Chromium
 * for some scripts, so no unit exceeds `maxTextUnitLength` unless a single
 * grapheme does. Units never split a grapheme, and their concatenation is the
 * input text.
 */
export const maxTextUnitLength = 2048
export const textUnitFrameBudget = maxTextUnitLength
const maxFrameBudget = 65_536
// Frame intervals that shrink or grow the next batch.
const slowFrameMs = 50
const fastFrameMs = 25
const softBreakWindow = 256

export interface TextUnit {
    readonly start: number
    readonly end: number
    readonly text: string
}

type Segments = ReturnType<Intl.Segmenter['segment']>

const defaultSegmenter =
    typeof Intl.Segmenter === 'function'
        ? new Intl.Segmenter(undefined, { granularity: 'grapheme' })
        : undefined

const extending = /^[\p{M}\p{Emoji_Modifier}\u200d\u{e0020}-\u{e007f}]/u

function isRegionalIndicator(text: string, index: number) {
    const code = text.codePointAt(index) ?? 0
    return code >= 0x1f1e6 && code <= 0x1f1ff
}

function isHangul(code: number) {
    return (
        (code >= 0x1100 && code <= 0x11ff) ||
        (code >= 0xa960 && code <= 0xa97f) ||
        (code >= 0xac00 && code <= 0xd7ff)
    )
}

/** Conservative boundary test for engines without Intl.Segmenter. */
export function isFallbackGraphemeBoundary(text: string, index: number) {
    if (index <= 0 || index >= text.length) return true
    const current = text.charCodeAt(index)
    const previous = text.charCodeAt(index - 1)
    if (
        current >= 0xdc00 &&
        current <= 0xdfff &&
        previous >= 0xd800 &&
        previous <= 0xdbff
    )
        return false
    if (previous === 0x0d && current === 0x0a) return false
    if (previous === 0x200d) return false
    if (extending.test(text.slice(index, index + 2))) return false
    // Conjoining jamo: V/T after any Hangul, anything Hangul after a leading L.
    if (current >= 0x1160 && current <= 0x11ff && isHangul(previous)) return false
    if (current >= 0xd7b0 && current <= 0xd7ff && isHangul(previous)) return false
    if (
        ((previous >= 0x1100 && previous <= 0x115f) ||
            (previous >= 0xa960 && previous <= 0xa97f)) &&
        isHangul(current)
    )
        return false
    if (isRegionalIndicator(text, index)) {
        let count = 0
        let cursor = index - 2
        while (cursor >= 0 && isRegionalIndicator(text, cursor)) {
            count++
            cursor -= 2
        }
        if (count % 2 === 1) return false
    }
    return true
}

/** Plans unit ends for one immutable text. */
export class TextUnitPlanner {
    readonly #text: string
    readonly #maxLength: number
    #segments: Segments | undefined
    readonly #segmenter: Intl.Segmenter | undefined
    #nextNewline = -1

    constructor(
        text: string,
        {
            maxLength = maxTextUnitLength,
            segmenter = defaultSegmenter,
        }: { maxLength?: number; segmenter?: Intl.Segmenter | null } = {},
    ) {
        this.#text = text
        this.#maxLength = Math.max(1, maxLength)
        this.#segmenter = segmenter ?? undefined
    }

    /** Last boundary at or before `index`, or the end of the grapheme at `floor`. */
    #boundary(index: number, floor: number) {
        const text = this.#text
        if (this.#segmenter) {
            this.#segments ??= this.#segmenter.segment(text)
            const segment = this.#segments.containing(index)
            if (!segment || segment.index === index) return index
            if (segment.index > floor) return segment.index
            return segment.index + segment.segment.length
        }
        for (let cursor = index; cursor > floor; cursor--) {
            if (isFallbackGraphemeBoundary(text, cursor)) return cursor
        }
        let cursor = index + 1
        while (cursor < text.length && !isFallbackGraphemeBoundary(text, cursor)) cursor++
        return cursor
    }

    isBoundary(index: number) {
        return this.#boundary(index, index - 1) === index
    }

    next(start: number) {
        const text = this.#text
        if (text.length - start <= this.#maxLength) return text.length
        const limit = start + this.#maxLength
        // Cache the forward newline so a text without newlines is scanned once.
        if (this.#nextNewline !== -2 && this.#nextNewline < start) {
            const found = text.indexOf('\n', start)
            this.#nextNewline = found === -1 ? -2 : found
        }
        if (this.#nextNewline >= 0 && this.#nextNewline < limit)
            return text.lastIndexOf('\n', limit - 1) + 1
        const lowest = Math.max(start + 1, limit - softBreakWindow)
        for (let cursor = limit - 1; cursor >= lowest; cursor--) {
            const code = text.charCodeAt(cursor)
            if (code === 0x20 || code === 0x09) return this.#boundary(cursor + 1, start)
        }
        return this.#boundary(limit, start)
    }
}

export interface TextUnitScheduler {
    request(callback: () => void): unknown
    cancel(handle: unknown): void
}

export const animationFrameScheduler: TextUnitScheduler = {
    request(callback) {
        return typeof requestAnimationFrame === 'function'
            ? { frame: requestAnimationFrame(callback) }
            : { timer: setTimeout(callback, 16) }
    },
    cancel(handle) {
        const scheduled = handle as {
            frame?: number
            timer?: ReturnType<typeof setTimeout>
        }
        if (scheduled.frame !== undefined) cancelAnimationFrame(scheduled.frame)
        if (scheduled.timer !== undefined) clearTimeout(scheduled.timer)
    },
}

/**
 * Publishes bounded units for the latest text, one budgeted batch
 * synchronously and the rest on later frames. The batch size follows the
 * measured frame interval. A new text keeps every unchanged unit except the
 * last, whose final grapheme may continue.
 */
export class IncrementalTextUnits {
    #text = ''
    #units: TextUnit[] = []
    #planner: TextUnitPlanner | undefined
    #pending: unknown
    #disposed = false
    readonly #publish: (units: readonly TextUnit[]) => void
    readonly #scheduler: TextUnitScheduler
    readonly #now: () => number
    #budget: number
    readonly #plannerOptions: ConstructorParameters<typeof TextUnitPlanner>[1]

    constructor(
        publish: (units: readonly TextUnit[]) => void,
        {
            scheduler = animationFrameScheduler,
            budget = textUnitFrameBudget,
            now = () => performance.now(),
            ...plannerOptions
        }: {
            scheduler?: TextUnitScheduler
            budget?: number
            now?: () => number
        } & NonNullable<ConstructorParameters<typeof TextUnitPlanner>[1]> = {},
    ) {
        this.#publish = publish
        this.#scheduler = scheduler
        this.#now = now
        this.#budget = Math.max(1, budget)
        this.#plannerOptions = plannerOptions
    }

    get budget() {
        return this.#budget
    }

    get complete() {
        return this.#end === this.#text.length
    }

    get #end() {
        return this.#units.at(-1)?.end ?? 0
    }

    setText(text: string) {
        if (this.#disposed || (text === this.#text && this.#planner)) return
        const planner = new TextUnitPlanner(text, this.#plannerOptions)
        const units = this.#units
        let keep = 0
        while (
            keep < units.length - 1 &&
            text.startsWith(units[keep].text, units[keep].start)
        )
            keep++
        // An edit right after a kept unit can make its end fall inside a grapheme.
        while (keep > 0 && !planner.isBoundary(units[keep - 1].end)) keep--
        units.length = keep
        this.#text = text
        this.#planner = planner
        this.#cancel()
        this.#pump()
    }

    dispose() {
        this.#disposed = true
        this.#cancel()
        this.#planner = undefined
    }

    #cancel() {
        if (this.#pending === undefined) return
        this.#scheduler.cancel(this.#pending)
        this.#pending = undefined
    }

    #pump() {
        const text = this.#text
        const planner = this.#planner!
        let start = this.#end
        let budget = this.#budget
        while (start < text.length && budget > 0) {
            const end = planner.next(start)
            this.#units.push({ start, end, text: text.slice(start, end) })
            budget -= end - start
            start = end
        }
        this.#publish(this.#units.slice())
        if (start < text.length) {
            const scheduled = this.#now()
            this.#pending = this.#scheduler.request(() => {
                this.#pending = undefined
                if (this.#disposed) return
                // The interval includes the style and layout of the previous batch.
                const interval = this.#now() - scheduled
                if (interval > slowFrameMs)
                    this.#budget = Math.max(1, Math.floor(this.#budget / 2))
                else if (interval < fastFrameMs)
                    this.#budget = Math.min(maxFrameBudget, this.#budget * 2)
                this.#pump()
            })
        }
    }
}
