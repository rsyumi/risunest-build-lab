import {
    animationFrameScheduler,
    maxTextUnitLength,
    TextUnitPlanner,
    textUnitFrameBudget,
    type TextUnitScheduler,
} from '../../ts/parser/boundedTextUnits'

interface ThoughtText {
    text: string
    target: HTMLSpanElement
    planner: TextUnitPlanner
    offset: number
}

export interface BoundedThoughtExpansion {
    readonly managed: ReadonlySet<HTMLDetailsElement>
    dispose(): void
}

/** Preserve Markdown elements while bounding the layout of oversized thought text. */
export function mountBoundedThoughtExpansion(
    root: HTMLElement,
    scheduler: TextUnitScheduler = animationFrameScheduler,
): BoundedThoughtExpansion {
    const managed = new Set<HTMLDetailsElement>()
    const cleanups: (() => void)[] = []
    for (const details of root.querySelectorAll<HTMLDetailsElement>('details[data-risu-thought]')) {
        const walker = document.createTreeWalker(details, NodeFilter.SHOW_TEXT)
        const oversized: Text[] = []
        while (walker.nextNode()) {
            const node = walker.currentNode as Text
            const parent = node.parentElement
            if (node.length <= maxTextUnitLength || !parent
                || parent.namespaceURI !== 'http://www.w3.org/1999/xhtml'
                || parent.closest('summary, style, script, textarea')
                || parent.closest('details[data-risu-thought]') !== details) continue
            oversized.push(node)
        }
        if (!oversized.length) continue
        managed.add(details)
        // Do not transfer an expanded preview into a full synchronous layout.
        details.open = false
        const texts: ThoughtText[] = oversized.map(node => {
            const target = document.createElement('span')
            target.dataset.risuThoughtText = ''
            target.style.display = 'contents'
            const text = node.data
            node.replaceWith(target)
            return { text, target, planner: new TextUnitPlanner(text), offset: 0 }
        })
        let disposed = false
        let pending: unknown
        let index = 0
        const cancel = () => {
            if (pending !== undefined) scheduler.cancel(pending)
            pending = undefined
        }
        const clear = () => {
            cancel()
            index = 0
            for (const text of texts) {
                text.offset = 0
                text.target.replaceChildren()
            }
        }
        const pump = () => {
            pending = undefined
            if (disposed || !details.open || !details.isConnected) return
            let budget = textUnitFrameBudget
            while (index < texts.length && budget > 0) {
                const text = texts[index]
                const end = text.planner.next(text.offset)
                const unit = document.createElement('span')
                unit.dataset.risuThoughtUnit = ''
                unit.style.display = 'inline-block'
                unit.style.width = '100%'
                unit.style.verticalAlign = 'top'
                unit.textContent = text.text.slice(text.offset, end)
                text.target.append(unit)
                budget -= end - text.offset
                text.offset = end
                if (end === text.text.length) index++
            }
            if (index < texts.length) pending = scheduler.request(pump)
        }
        const toggle = () => {
            if (details.open) {
                if (pending === undefined && index < texts.length) pump()
            } else clear()
        }
        details.addEventListener('toggle', toggle)
        cleanups.push(() => {
            disposed = true
            details.removeEventListener('toggle', toggle)
            clear()
        })
    }
    return {
        managed,
        dispose() {
            for (const cleanup of cleanups.splice(0)) cleanup()
            managed.clear()
        },
    }
}
