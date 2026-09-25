import { mount, unmount } from 'svelte'
import Harness from '../../src/lib/ChatScreens/StreamingThoughtPreviewHarness.test.svelte'
import { getStreamingThoughtPreview } from '../../src/ts/parser/streamingThoughtPreview'
import {
    asciiControlText,
    repeatedKoreanEmojiThought,
    wrapThought,
} from '../../src/ts/parser/largeThoughtFixtures.testUtils'

// Mirrors the chat message column and the shared thought styles in src/styles.css.
const css = `
.thought-column { width: 100%; max-width: 393px; padding: 0 0.5rem; box-sizing: border-box; font-family: Arial, sans-serif, serif; }
.thought-column .chattext { font-size: 0.875rem; line-height: 1.25rem; max-width: calc(100% - 0.5rem); word-break: normal; overflow-wrap: anywhere; }
.whitespace-pre-wrap { white-space: pre-wrap; }
.x-risu-streaming-thought-preview { display: block; margin: 0.5em 0; padding: 0.5em 0.75em; border: 1px solid #444; border-radius: 0.5rem; }
`

let active: ReturnType<typeof mount> | undefined
let expected = ''
let toggles: { open: boolean; at: number }[] = []
// The largest gap between animation frames covers script, style, and layout.
let maxFrameGap = 0
let monitor = 0
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

function column() {
    let root = document.querySelector<HTMLElement>('#thought')
    if (!root) {
        const style = document.createElement('style')
        style.textContent = css
        document.head.append(style)
        root = document.createElement('div')
        root.id = 'thought'
        root.className = 'thought-column'
        document.body.append(root)
    }
    return root
}

export const thoughtDriver = {
    mount(kind: 'repeated' | 'ascii') {
        if (active) void unmount(active)
        const root = column()
        root.replaceChildren()
        const text = root.appendChild(document.createElement('span'))
        text.className = 'text chat-width chattext prose minw-0'
        const source = wrapThought(
            kind === 'repeated' ? repeatedKoreanEmojiThought() : asciiControlText(500_000),
        )
        expected = getStreamingThoughtPreview(source, true)!.full!
        active = mount(Harness, { target: text, props: { initialSource: source } })
        const details = text.querySelector('details')!
        details.addEventListener('toggle', () => toggles.push({ open: details.open, at: performance.now() }))
        toggles = []
        watchFrames()
        return { sourceLength: source.length, expectedLength: expected.length }
    },
    toggle() {
        const at = performance.now()
        document.querySelector<HTMLElement>('#thought summary')!.click()
        return at
    },
    get state() {
        const body = document.querySelector('#thought [data-incremental-plain-text]')
        const text = body?.textContent ?? ''
        return {
            open: !!document.querySelector<HTMLDetailsElement>('#thought details')?.open,
            units: body?.childElementCount ?? 0,
            length: text.length,
            complete: text === expected,
            endsWithLatest: text.endsWith('LATEST-SYNTHETIC'),
            maxFrameGap,
            toggles,
        }
    },
    frames(count: number) {
        return new Promise<number>((resolve) => {
            const started = performance.now()
            const step = (left: number) =>
                left ? requestAnimationFrame(() => step(left - 1)) : resolve(performance.now() - started)
            step(count)
        })
    },
}
