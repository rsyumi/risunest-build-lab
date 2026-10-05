import { afterEach, describe, expect, test } from 'vitest'
import { mountBoundedThoughtExpansion } from './boundedThoughtExpansion'
import { maxTextUnitLength, type TextUnitScheduler } from '../../ts/parser/boundedTextUnits'

class Frames implements TextUnitScheduler {
    pending = new Map<number, () => void>()
    next = 0
    request(callback: () => void) {
        const id = ++this.next
        this.pending.set(id, callback)
        return id
    }
    cancel(handle: unknown) { this.pending.delete(handle as number) }
    flush() {
        for (let count = 0; this.pending.size; count++) {
            if (count > 100) throw new Error('Expansion did not finish')
            const [id, callback] = this.pending.entries().next().value!
            this.pending.delete(id)
            callback()
        }
    }
}

const cleanups: (() => void)[] = []
afterEach(() => {
    for (const cleanup of cleanups.splice(0)) cleanup()
    document.body.replaceChildren()
})

function fixture(html: string) {
    const root = document.createElement('div')
    root.innerHTML = html
    document.body.append(root)
    const frames = new Frames()
    const expansion = mountBoundedThoughtExpansion(root, frames)
    cleanups.push(() => expansion.dispose())
    const details = root.querySelector<HTMLDetailsElement>('details')!
    const toggle = (open: boolean) => {
        details.open = open
        details.dispatchEvent(new Event('toggle'))
    }
    return { root, frames, expansion, details, toggle }
}

function withoutLayoutUnits(root: HTMLElement) {
    const copy = root.cloneNode(true) as HTMLElement
    for (const unit of copy.querySelectorAll('[data-risu-thought-text], [data-risu-thought-unit]')) {
        unit.replaceWith(...unit.childNodes)
    }
    copy.normalize()
    return copy
}

describe('bounded settled thought expansion', () => {
    test('keeps Markdown elements, links, exact text, and existing media nodes', () => {
        const text = '합성 e\u0301 👨‍👩‍👧‍👦 '.repeat(400)
        const html = `<summary>Thought</summary><p><strong>${text}</strong><a href="https://example.invalid/synthetic">${text}</a></p><img data-risu-inlay-token="synthetic">`
        const { details, frames, toggle } = fixture(`<details data-risu-thought>${html}</details>`)
        const link = details.querySelector('a')!
        const media = details.querySelector('img')!
        expect(details.querySelectorAll('[data-risu-thought-unit]')).toHaveLength(0)
        toggle(true)
        expect(details.textContent!.length).toBeLessThan(2 * text.length)
        frames.flush()
        expect(withoutLayoutUnits(details).innerHTML).toBe(html)
        expect(details.querySelector('a')).toBe(link)
        expect(link.getAttribute('href')).toBe('https://example.invalid/synthetic')
        expect(details.querySelector('img')).toBe(media)
        for (const unit of details.querySelectorAll('[data-risu-thought-unit]')) {
            expect(unit.textContent!.length).toBeLessThanOrEqual(maxTextUnitLength)
        }
        expect(details.querySelector('strong')!.textContent).toBe(text)
    })

    test('collapsing cancels work, removes partial units and restarts exactly on reopen', () => {
        const text = 'synthetic '.repeat(1500)
        const { details, frames, toggle } = fixture(`<details data-risu-thought><summary>Thought</summary>${text}</details>`)
        toggle(true)
        expect(frames.pending.size).toBe(1)
        toggle(false)
        expect(frames.pending.size).toBe(0)
        expect(details.querySelectorAll('[data-risu-thought-unit]')).toHaveLength(0)
        frames.flush()
        expect(details.textContent).toBe('Thought')
        toggle(true)
        frames.flush()
        expect(details.textContent).toBe(`Thought${text}`)
    })

    test('disposal cancels even a previously captured callback and leaves media for its owner', () => {
        const { details, frames, expansion, toggle } = fixture(`<details data-risu-thought><summary>Thought</summary>${'x'.repeat(6000)}<img data-risu-inlay-token="synthetic"></details>`)
        const media = details.querySelector('img')!
        toggle(true)
        const late = frames.pending.values().next().value!
        expansion.dispose()
        late()
        expect(frames.pending.size).toBe(0)
        expect(details.querySelectorAll('[data-risu-thought-unit]')).toHaveLength(0)
        expect(details.querySelector('img')).toBe(media)
        toggle(false)
        toggle(true)
        expect(frames.pending.size).toBe(0)
    })

    test('only oversized body text suppresses automatic reopening', () => {
        const { root, details, expansion } = fixture(`<details data-risu-thought open><summary>${'s'.repeat(6000)}</summary>${'x'.repeat(maxTextUnitLength)}</details><details data-risu-thought open><summary>Large</summary>${'y'.repeat(maxTextUnitLength + 1)}</details>`)
        const large = root.querySelectorAll<HTMLDetailsElement>('details')[1]
        expect(expansion.managed.has(details)).toBe(false)
        expect(details.open).toBe(true)
        expect(expansion.managed.has(large)).toBe(true)
        expect(large.open).toBe(false)
    })
})
