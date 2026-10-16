export interface RepresentativeImagePriority {
    ordered: string[]
    visible: string[]
}

const lists = new Set<RepresentativeImageList>()
const listeners = new Set<() => void>()
let pending = false
let snapshot: RepresentativeImagePriority | undefined
let snapshotCurrent = false

function changed(): void {
    snapshotCurrent = false
    if (pending) return
    pending = true
    queueMicrotask(() => {
        pending = false
        for (const listener of listeners) listener()
    })
}

export function readRepresentativeImagePriority(): RepresentativeImagePriority | undefined {
    if (snapshotCurrent) return snapshot
    const active = [...lists].map(list => ({ list, visible: list.visibleReferences() }))
        .sort((a, b) => Number(b.visible.size > 0) - Number(a.visible.size > 0))
    const ordered = [...new Set(active.flatMap(({ list }) => list.customOrder ? list.order : []))]
    const visible = [...new Set(active.flatMap(({ list, visible }) => list.order.filter(reference => visible.has(reference))))]
    snapshot = ordered.length || visible.length ? { ordered, visible } : undefined
    snapshotCurrent = true
    return snapshot
}

export function subscribeRepresentativeImagePriority(listener: () => void): () => void {
    listeners.add(listener)
    return () => { listeners.delete(listener) }
}

export class RepresentativeImageList {
    order: string[] = []
    private rows = new Map<Element, { reference: string; visible: boolean }>()
    private observer: IntersectionObserver | undefined

    constructor(public customOrder = true) { lists.add(this) }

    setOrder(references: Array<string | null | undefined>, customOrder = this.customOrder): void {
        const next = [...new Set(references.filter((reference): reference is string => !!reference))]
        if (customOrder === this.customOrder && next.length === this.order.length && next.every((reference, index) => reference === this.order[index])) return
        this.customOrder = customOrder
        this.order = next
        changed()
    }

    visibleReferences(): Set<string> {
        return new Set([...this.rows.values()].filter(row => row.visible).map(row => row.reference))
    }

    observe(element: Element, reference: string | null | undefined): () => void {
        if (!reference) return () => {}
        this.rows.set(element, { reference, visible: false })
        if (typeof IntersectionObserver !== 'undefined') {
            this.observer ??= new IntersectionObserver(entries => {
                let updated = false
                for (const entry of entries) {
                    const row = this.rows.get(entry.target)
                    if (!row) continue
                    const visible = entry.isIntersecting
                    if (row.visible !== visible) { row.visible = visible; updated = true }
                }
                if (updated) changed()
            }, { root: null, rootMargin: '0px', threshold: 0 })
            this.observer.observe(element)
        }
        return () => {
            const row = this.rows.get(element)
            this.rows.delete(element)
            this.observer?.unobserve(element)
            if (row?.visible) changed()
        }
    }

    dispose(): void {
        this.observer?.disconnect()
        this.rows.clear()
        lists.delete(this)
        changed()
    }
}
