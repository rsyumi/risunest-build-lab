export function reconcileChatViewportChildren(parent: HTMLElement, ordered: readonly HTMLElement[]): void {
    const retained = new Set<Node>(ordered)
    let cursor = parent.firstChild
    // Install replacement gaps before removing rows so the scroll range never collapses during teardown.
    for (const element of ordered) {
        while (cursor && !retained.has(cursor)) cursor = cursor.nextSibling
        if (cursor === element) cursor = cursor.nextSibling
        else parent.insertBefore(element, cursor)
    }
    for (const child of [...parent.childNodes]) {
        if (!retained.has(child)) child.remove()
    }
}
