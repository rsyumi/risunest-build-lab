type NearViewportCallback = () => void

type ObserverGroup = {
    root: Element | null
    observer: IntersectionObserver
    callbacks: Map<Element, NearViewportCallback>
}
type Registration = { group: ObserverGroup; callback: NearViewportCallback }
const groups = new Map<Element | null, ObserverGroup>()
const registrations = new WeakMap<Element, Registration>()

function remove(element: Element, registration: Registration) {
    if (registrations.get(element) !== registration) return
    registrations.delete(element)
    const { group } = registration
    group.callbacks.delete(element)
    group.observer.unobserve(element)
    if (group.callbacks.size === 0) {
        group.observer.disconnect()
        groups.delete(group.root)
    }
}

function getObserver(root: Element | null): ObserverGroup {
    const existing = groups.get(root)
    if (existing) return existing
    const group: ObserverGroup = {
        root,
        callbacks: new Map(),
        observer: new IntersectionObserver((entries) => {
            for (const entry of entries) {
                if (!entry.isIntersecting) continue
                const registration = registrations.get(entry.target)
                if (!registration || registration.group !== group) continue
                remove(entry.target, registration)
                registration.callback()
            }
        }, { root, rootMargin: '240px', threshold: 0 }),
    }
    groups.set(root, group)
    return group
}

export function observeNearViewport(
    element: Element,
    callback: NearViewportCallback,
    root: Element | null = null,
): () => void {
    if (typeof IntersectionObserver === 'undefined') {
        callback()
        return () => {}
    }
    const previous = registrations.get(element)
    if (previous?.group.root === root) previous.group.observer.unobserve(element)
    else if (previous) remove(element, previous)
    const group = getObserver(root)
    const registration = { group, callback }
    registrations.set(element, registration)
    group.callbacks.set(element, callback)
    group.observer.observe(element)
    return () => remove(element, registration)
}
