// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from 'vitest'
import { RepresentativeImageList, readRepresentativeImagePriority, subscribeRepresentativeImagePriority } from './representativeImagePriority'

class Observer {
    static all: Observer[] = []
    observed = new Set<Element>()
    constructor(readonly callback: IntersectionObserverCallback, readonly options: IntersectionObserverInit) { Observer.all.push(this) }
    observe(element: Element) { this.observed.add(element) }
    unobserve(element: Element) { this.observed.delete(element) }
    disconnect() { this.observed.clear() }
    visible(element: Element, visible: boolean, ratio = visible ? 1 : 0) {
        this.callback([{ target: element, isIntersecting: visible, intersectionRatio: ratio } as IntersectionObserverEntry], this as unknown as IntersectionObserver)
    }
}
const lists: RepresentativeImageList[] = []
afterEach(() => { lists.splice(0).forEach(list => list.dispose()); Observer.all = []; vi.unstubAllGlobals() })

it('keeps display order and current visible references across scrolling, duplicate images and unmount', async () => {
    vi.stubGlobal('IntersectionObserver', Observer)
    const list = new RepresentativeImageList(); lists.push(list)
    list.setOrder(['assets/z', 'assets/folder', 'assets/a', 'assets/z'])
    const first = document.createElement('div'), last = document.createElement('div')
    const stopFirst = list.observe(first, 'assets/z'), stopLast = list.observe(last, 'assets/a')
    const changed = vi.fn(), unsubscribe = subscribeRepresentativeImagePriority(changed)
    const observer = Observer.all[0]
    expect(observer.options.rootMargin).toBe('0px')
    observer.visible(last, true, 0)
    await Promise.resolve()
    expect(readRepresentativeImagePriority()).toEqual({ ordered: ['assets/z', 'assets/folder', 'assets/a'], visible: ['assets/a'] })
    observer.visible(last, false); observer.visible(first, true)
    await Promise.resolve()
    expect(readRepresentativeImagePriority()?.visible).toEqual(['assets/z'])
    expect(observer.observed.has(last)).toBe(true)
    stopFirst(); stopLast(); await Promise.resolve()
    expect(readRepresentativeImagePriority()?.visible).toEqual([])
    expect(changed).toHaveBeenCalledTimes(3)
    unsubscribe(); list.dispose()
    expect(readRepresentativeImagePriority()).toBeUndefined()
})

it('uses the currently visible list order without confusing mounted rows with visible rows', () => {
    vi.stubGlobal('IntersectionObserver', Observer)
    const desktop = new RepresentativeImageList(), mobile = new RepresentativeImageList(); lists.push(desktop, mobile)
    desktop.setOrder(['assets/a', 'assets/b']); mobile.setOrder(['assets/b', 'assets/a'])
    const row = document.createElement('button'); mobile.observe(row, 'assets/b')
    expect(readRepresentativeImagePriority()?.visible).toEqual([])
    Observer.all[0].visible(row, true)
    expect(readRepresentativeImagePriority()).toEqual({ ordered: ['assets/b', 'assets/a'], visible: ['assets/b'] })
    mobile.setOrder(['assets/a'])
    expect(readRepresentativeImagePriority()?.visible).toEqual([])
    mobile.dispose()
    expect(readRepresentativeImagePriority()?.ordered).toEqual(['assets/a', 'assets/b'])
})

it('sends only visible desktop aliases and reuses settled priority snapshots', () => {
    vi.stubGlobal('IntersectionObserver', Observer)
    const desktop = new RepresentativeImageList(false); lists.push(desktop)
    const references = Array.from({ length: 1000 }, (_, index) => `assets/${index}`)
    desktop.setOrder(references)
    expect(readRepresentativeImagePriority()).toBeUndefined()
    const row = document.createElement('button'); desktop.observe(row, references[8])
    Observer.all[0].visible(row, true)
    const priority = readRepresentativeImagePriority()
    expect(priority).toEqual({ ordered: [], visible: [references[8]] })
    expect(readRepresentativeImagePriority()).toBe(priority)
    Observer.all[0].visible(row, false)
    expect(readRepresentativeImagePriority()).toBeUndefined()
    desktop.setOrder(['assets/folder', references[8]], true)
    expect(readRepresentativeImagePriority()).toEqual({ ordered: ['assets/folder', references[8]], visible: [] })
    desktop.setOrder(references, false)
    expect(readRepresentativeImagePriority()).toBeUndefined()
})
