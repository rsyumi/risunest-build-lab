import { afterEach, describe, expect, it, vi } from 'vitest'

class TestIntersectionObserver {
    static instances: TestIntersectionObserver[] = []

    readonly observed = new Set<Element>()
    readonly unobserve = vi.fn((element: Element) =>
        this.observed.delete(element),
    )
    readonly disconnect = vi.fn(() => this.observed.clear())

    constructor(
        private readonly callback: IntersectionObserverCallback,
        readonly options?: IntersectionObserverInit,
    ) {
        TestIntersectionObserver.instances.push(this)
    }

    observe(element: Element) {
        this.observed.add(element)
    }

    setVisible(...elements: Element[]) {
        this.callback(
            elements.map(
                (target) =>
                    ({
                        target,
                        isIntersecting: true,
                    }) as IntersectionObserverEntry,
            ),
            this as unknown as IntersectionObserver,
        )
    }
}

afterEach(() => {
    TestIntersectionObserver.instances = []
    vi.unstubAllGlobals()
    vi.resetModules()
})

describe('observeNearViewport', () => {
    it('shares one observer and invokes only intersecting elements once', async () => {
        vi.stubGlobal('IntersectionObserver', TestIntersectionObserver)
        const { observeNearViewport } = await import('./observeNearViewport')
        const first = document.createElement('div')
        const second = document.createElement('div')
        const firstCallback = vi.fn()
        const secondCallback = vi.fn()

        const stopFirst = observeNearViewport(first, firstCallback)
        const stopSecond = observeNearViewport(second, secondCallback)
        const observer = TestIntersectionObserver.instances[0]

        expect(TestIntersectionObserver.instances).toHaveLength(1)
        expect(observer.options).toMatchObject({
            root: null,
            rootMargin: '240px',
            threshold: 0,
        })
        observer.setVisible(first)
        observer.setVisible(first)

        expect(firstCallback).toHaveBeenCalledTimes(1)
        expect(secondCallback).not.toHaveBeenCalled()
        expect(observer.unobserve).toHaveBeenCalledWith(first)

        stopFirst()
        stopSecond()
        expect(observer.disconnect).toHaveBeenCalledTimes(1)
    })

    it('removes a pending callback during cleanup', async () => {
        vi.stubGlobal('IntersectionObserver', TestIntersectionObserver)
        const { observeNearViewport } = await import('./observeNearViewport')
        const element = document.createElement('div')
        const callback = vi.fn()

        const stop = observeNearViewport(element, callback)
        const observer = TestIntersectionObserver.instances[0]
        stop()
        observer.setVisible(element)

        expect(callback).not.toHaveBeenCalled()
        expect(observer.disconnect).toHaveBeenCalledTimes(1)
    })

    it('isolates scrolling roots and releases each pool after its last pending target', async () => {
        vi.stubGlobal('IntersectionObserver', TestIntersectionObserver)
        const { observeNearViewport } = await import('./observeNearViewport')
        const firstRoot = document.createElement('div'), secondRoot = document.createElement('div')
        const first = document.createElement('div'), second = document.createElement('div'), third = document.createElement('div')
        const firstCallback = vi.fn(), secondCallback = vi.fn(), thirdCallback = vi.fn()
        const stopFirst = observeNearViewport(first, firstCallback, firstRoot)
        const stopSecond = observeNearViewport(second, secondCallback, firstRoot)
        const stopThird = observeNearViewport(third, thirdCallback, secondRoot)
        expect(TestIntersectionObserver.instances).toHaveLength(2)
        const [firstObserver, secondObserver] = TestIntersectionObserver.instances
        firstObserver.setVisible(first, third)
        expect(firstCallback).toHaveBeenCalledOnce()
        expect(thirdCallback).not.toHaveBeenCalled()
        expect(firstObserver.observed.has(second)).toBe(true)
        stopSecond()
        expect(firstObserver.observed.size).toBe(0)
        expect(firstObserver.disconnect).toHaveBeenCalledOnce()
        expect(secondObserver.observed.has(third)).toBe(true)
        secondObserver.setVisible(third)
        expect(thirdCallback).toHaveBeenCalledOnce()
        expect(secondObserver.disconnect).toHaveBeenCalledOnce()
        stopFirst(); stopThird()
        const stopAgain = observeNearViewport(first, firstCallback, firstRoot)
        const nextObserver = TestIntersectionObserver.instances[2]
        firstObserver.setVisible(first)
        expect(firstCallback).toHaveBeenCalledOnce()
        nextObserver.setVisible(first)
        expect(firstCallback).toHaveBeenCalledTimes(2)
        stopAgain()
    })

    it('does not let an old root or cleanup consume a replacement registration', async () => {
        vi.stubGlobal('IntersectionObserver', TestIntersectionObserver)
        const { observeNearViewport } = await import('./observeNearViewport')
        const element = document.createElement('div'), root = document.createElement('div')
        const callback = vi.fn()
        const stopOld = observeNearViewport(element, callback)
        const oldObserver = TestIntersectionObserver.instances[0]
        const stopNew = observeNearViewport(element, callback, root)
        const nextObserver = TestIntersectionObserver.instances[1]
        stopOld(); oldObserver.setVisible(element)
        expect(callback).not.toHaveBeenCalled()
        expect(nextObserver.observed.has(element)).toBe(true)
        nextObserver.setVisible(element)
        expect(callback).toHaveBeenCalledOnce()
        stopNew()
    })

    it('loads immediately when IntersectionObserver is unavailable', async () => {
        vi.stubGlobal('IntersectionObserver', undefined)
        const { observeNearViewport } = await import('./observeNearViewport')
        const callback = vi.fn()

        const stop = observeNearViewport(
            document.createElement('div'),
            callback,
        )

        expect(callback).toHaveBeenCalledTimes(1)
        expect(() => stop()).not.toThrow()
    })
})
