import { mount, unmount } from 'svelte'
import Harness from './SettledThoughtHarness.svelte'
import { ParseMarkdown } from '../../src/ts/parser/parser.svelte'
import { repeatedKoreanEmojiThought } from '../../src/ts/parser/largeThoughtFixtures.testUtils'

export type SettledThoughtShape = 'no-newline' | 'periodic-break' | 'paragraph'

// Match the existing expansion fixture's mobile chat column and thought styles.
const style = document.createElement('style')
style.textContent = `
.thought-column { width: 100%; max-width: 393px; padding: 0 0.5rem; box-sizing: border-box; font-family: Arial, sans-serif, serif; }
.thought-column .chattext { font-size: 0.875rem; line-height: 1.25rem; max-width: calc(100% - 0.5rem); word-break: normal; overflow-wrap: anywhere; }
.whitespace-pre-wrap { white-space: pre-wrap; }
.x-risu-streaming-thought-preview { display: block; margin: 0.5em 0; padding: 0.5em 0.75em; border: 1px solid #444; border-radius: 0.5rem; }
`
document.head.append(style)
const root = document.querySelector<HTMLElement>('#thought')!
let active: { settle(): void } | undefined
let expectedText = ''
let expectedHtml = ''
let source = ''
let settled = false
let error = false
let maxFrameGap = 0
let monitor = 0
let readyAt = 0
let parseMs = 0
let toggleHandlerMs = 0

function watchFrames() {
    cancelAnimationFrame(monitor)
    maxFrameGap = 0
    let last = performance.now()
    const step = (now: number) => {
        maxFrameGap = Math.max(maxFrameGap, now - last)
        last = now
        monitor = requestAnimationFrame(step)
    }
    monitor = requestAnimationFrame(step)
}

function thoughtBody(details: HTMLDetailsElement) {
    const copy = details.cloneNode(true) as HTMLDetailsElement
    copy.querySelector('summary')?.remove()
    copy.removeAttribute('open')
    for (const unit of copy.querySelectorAll('[data-risu-thought-text], [data-risu-thought-unit]')) {
        unit.replaceWith(...unit.childNodes)
    }
    copy.normalize()
    return copy
}

export const settledThought = {
    async mount(shape: SettledThoughtShape, previewInitially = false, units = 500_000) {
        if (active) await unmount(active)
        root.replaceChildren()
        const segment = repeatedKoreanEmojiThought(shape === 'periodic-break' ? 30 : 100)
        const separator = shape === 'no-newline' ? '' : shape === 'periodic-break' ? '<br>' : '\n\n'
        const body = Array.from({ length: Math.ceil(units / segment.length) }, () => segment).join(separator)
        source = `<Thoughts><strong>SYNTHETIC-BOLD</strong> <a href="https://example.invalid/synthetic">SYNTHETIC-LINK</a> ${body}LATEST-SYNTHETIC</Thoughts>\n\n**Answer**\n\n[Answer link](https://example.invalid/answer)`
        const parseStarted = performance.now()
        const html = await ParseMarkdown(source, null)
        parseMs = performance.now() - parseStarted
        const reference = document.createElement('template')
        reference.innerHTML = html
        const referenceDetails = reference.content.querySelector<HTMLDetailsElement>('details[data-risu-thought]')!
        const referenceBody = thoughtBody(referenceDetails)
        expectedText = referenceBody.textContent ?? ''
        expectedHtml = referenceBody.innerHTML
        settled = false
        error = false
        readyAt = 0
        toggleHandlerMs = 0
        active = mount(Harness, { target: root, props: {
            source, previewInitially,
            onSettled: () => { settled = true; readyAt = performance.now() },
            onError: () => { error = true },
        } })
        watchFrames()
        return {
            sourceLength: source.length, expectedLength: expectedText.length, parseMs,
            sourceContentPreserved: expectedText.replaceAll(/\s/g, '') ===
                `SYNTHETIC-BOLD SYNTHETIC-LINK ${body}LATEST-SYNTHETIC`.replaceAll('<br>', '').replaceAll(/\s/g, ''),
        }
    },
    toggle() {
        const at = performance.now()
        root.querySelector<HTMLElement>('summary')!.click()
        toggleHandlerMs = performance.now() - at
        return at
    },
    settle() {
        watchFrames()
        const at = performance.now()
        active!.settle()
        return at
    },
    resetTiming: watchFrames,
    frames(count: number) {
        return new Promise<void>((resolve) => {
            const step = (left: number) => left ? requestAnimationFrame(() => step(left - 1)) : resolve()
            step(count)
        })
    },
    get state() {
        const details = root.querySelector<HTMLDetailsElement>('details[data-risu-thought]')
        const body = details ? thoughtBody(details) : null
        const text = body?.textContent ?? ''
        return {
            settled, error, readyAt, parseMs, maxFrameGap, toggleHandlerMs,
            open: !!details?.open,
            previewOpen: !!root.querySelector<HTMLDetailsElement>('details[data-streaming-thought-preview]')?.open,
            hasPreview: !!root.querySelector('[data-streaming-thought-preview]'),
            exactText: text === expectedText, exactMarkup: body?.innerHTML === expectedHtml,
            length: text.length, endsWithLatest: text.trimEnd().endsWith('LATEST-SYNTHETIC'),
            thoughtBold: body?.querySelector('strong')?.textContent,
            thoughtLink: body?.querySelector('a')?.getAttribute('href'),
            answerBold: Array.from(root.querySelectorAll('strong')).some(node => node.textContent === 'Answer'),
            answerLink: !!root.querySelector('a[href="https://example.invalid/answer"]'),
            elementCount: details?.querySelectorAll('*').length ?? 0,
            units: details?.querySelectorAll('[data-risu-thought-unit]').length ?? 0,
        }
    },
}
Object.assign(window, { settledThought })
