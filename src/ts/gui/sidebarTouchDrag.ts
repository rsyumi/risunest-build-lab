const HOLD_MS = 400
const MOVE_PX = 8
const SOURCE = '[data-sidebar-drag-id]'
const TARGET = '[data-sidebar-drop]'

/** Touch input shares the sidebar's existing HTML drag/drop handlers. */
export function sidebarTouchDrag(node: HTMLElement) {
    type Gesture = {
        source: HTMLElement
        origin: Element
        identity: string
        identifier: number
        started: number
        startX: number
        startY: number
        x: number
        y: number
        scrolling: boolean
        data: DataTransfer | null
        ghost: HTMLElement | null
        target: Element | null
        accepted: boolean
    }
    let gesture: Gesture | null = null
    let frame = 0
    let suppressClickUntil = 0
    let previousFrame = 0
    const touchStyles = ['-webkit-touch-callout', '-webkit-user-select', 'user-select'].map(name => ({
        name, value: node.style.getPropertyValue(name), priority: node.style.getPropertyPriority(name),
    }))
    // Native image callouts and text selection can take over a stationary hold on iOS.
    for (const { name } of touchStyles) node.style.setProperty(name, 'none')

    function valid(g: Gesture) {
        return node.isConnected && node.getClientRects().length > 0 && node.contains(g.source)
            && g.source.dataset.sidebarDragId === g.identity
    }

    function dispatch(g: Gesture, type: string, target: Element) {
        return !target.dispatchEvent(new DragEvent(type, {
            bubbles: true, cancelable: true, dataTransfer: g.data,
            clientX: g.x, clientY: g.y,
        }))
    }

    function hover(g: Gesture) {
        const target = document.elementFromPoint(g.x, g.y)?.closest(TARGET) ?? null
        const next = target && node.contains(target) ? target : null
        if (g.target !== next) {
            if (g.target) dispatch(g, 'dragleave', g.target)
            g.target = next
            if (next) dispatch(g, 'dragenter', next)
        }
        g.accepted = next !== null && dispatch(g, 'dragover', next)
    }

    function finish(drop = false) {
        const g = gesture
        if (!g) return
        gesture = null
        cancelAnimationFrame(frame)
        frame = 0
        try {
            if (g.data) {
                if (drop && valid(g)) {
                    hover(g)
                    if (g.target && g.accepted) dispatch(g, 'drop', g.target)
                }
                if (g.target) dispatch(g, 'dragleave', g.target)
                dispatch(g, 'dragend', g.source)
            }
        } finally {
            g.ghost?.remove()
            if (g.data || g.scrolling || performance.now() - g.started >= HOLD_MS) {
                suppressClickUntil = performance.now() + 700
            }
        }
    }

    function animate(now: number) {
        const g = gesture
        if (!g?.data) return
        if (!valid(g)) { finish(); return }
        const bounds = node.getBoundingClientRect()
        const elapsed = Math.min(32, now - previousFrame)
        previousFrame = now
        if (g.x >= bounds.left && g.x <= bounds.right && g.y >= bounds.top && g.y <= bounds.bottom) {
            const edge = Math.min(36, bounds.height / 4)
            const direction = g.y < bounds.top + edge ? -1 : g.y > bounds.bottom - edge ? 1 : 0
            node.scrollTop += direction * elapsed * 0.45
        }
        hover(g)
        frame = requestAnimationFrame(animate)
    }

    function startDrag(g: Gesture) {
        if (!valid(g)) { finish(); return false }
        g.data = new DataTransfer()
        g.data.effectAllowed = 'move'
        if (dispatch(g, 'dragstart', g.source)) { finish(); return false }
        const avatar = g.source.querySelector<HTMLElement>('.avatar') ?? g.source
        const bounds = avatar.getBoundingClientRect()
        g.ghost = avatar.cloneNode(true) as HTMLElement
        g.ghost.removeAttribute('id')
        g.ghost.setAttribute('aria-hidden', 'true')
        g.ghost.dataset.sidebarDragGhost = ''
        Object.assign(g.ghost.style, {
            position: 'fixed', top: '0', left: '0', width: `${bounds.width}px`, height: `${bounds.height}px`,
            margin: '0', pointerEvents: 'none', opacity: '0.7', zIndex: '2147483647',
        })
        for (const child of g.ghost.querySelectorAll<HTMLElement>('*')) child.style.pointerEvents = 'none'
        document.body.append(g.ghost)
        previousFrame = performance.now()
        frame = requestAnimationFrame(animate)
        return true
    }

    function start(event: TouchEvent) {
        const origin = event.target instanceof Element ? event.target : null
        const source = origin?.closest<HTMLElement>(SOURCE)
        if (!source || !node.contains(source)) return
        // Keep the legacy document-level mobile polyfill out of this gesture.
        event.stopPropagation()
        finish()
        if (event.touches.length !== 1 || !source.dataset.sidebarDragId) return
        const touch = event.touches[0]
        suppressClickUntil = 0
        gesture = {
            source, origin, identity: source.dataset.sidebarDragId, identifier: touch.identifier,
            started: performance.now(), startX: touch.clientX, startY: touch.clientY,
            x: touch.clientX, y: touch.clientY, scrolling: false,
            data: null, ghost: null, target: null, accepted: false,
        }
    }

    function multitouch(event: TouchEvent) {
        if (gesture && event.touches.length !== 1) finish()
    }

    function move(event: TouchEvent) {
        const g = gesture
        if (!g) return
        const touch = Array.from(event.touches).find(t => t.identifier === g.identifier)
        if (event.touches.length !== 1 || !touch || !valid(g)) { finish(); return }
        g.x = touch.clientX
        g.y = touch.clientY
        if (g.scrolling) return
        const moved = Math.hypot(g.x - g.startX, g.y - g.startY) > MOVE_PX
        if (!g.data) {
            if (performance.now() - g.started < HOLD_MS) {
                if (moved) g.scrolling = true
                return
            }
            if (!moved) return
            if (!event.cancelable) { finish(); return }
            if (!startDrag(g)) return
        }
        event.preventDefault()
        g.ghost!.style.transform = `translate(${g.x - 10}px, ${g.y - 10}px)`
        hover(g)
    }

    function end(event: TouchEvent) {
        const g = gesture
        const touch = g && Array.from(event.changedTouches).find(t => t.identifier === g.identifier)
        if (!g || !touch) return
        g.x = touch.clientX
        g.y = touch.clientY
        const held = !g.scrolling && performance.now() - g.started >= HOLD_MS
        const contextTarget = held && !g.data && valid(g) ? g.origin : null
        if (g.data || held) event.preventDefault()
        finish(true)
        // A stationary hold retains the folder's existing context menu.
        contextTarget?.dispatchEvent(new MouseEvent('contextmenu', {
            bubbles: true, cancelable: true, clientX: g.x, clientY: g.y,
        }))
    }

    function cancel() { finish() }
    function visibility() { if (document.hidden) finish() }
    function nativeDrag(event: DragEvent) {
        if (gesture && event.isTrusted) { event.preventDefault(); event.stopPropagation() }
    }
    function contextmenu(event: MouseEvent) {
        if (gesture) { event.preventDefault(); event.stopPropagation() }
    }
    function click(event: MouseEvent) {
        if (event.detail !== 0 && performance.now() < suppressClickUntil) {
            event.preventDefault()
            event.stopPropagation()
        }
    }

    node.addEventListener('touchstart', start, { passive: true })
    node.addEventListener('dragstart', nativeDrag, true)
    node.addEventListener('contextmenu', contextmenu, true)
    node.addEventListener('click', click, true)
    document.addEventListener('touchstart', multitouch, { passive: true, capture: true })
    document.addEventListener('touchmove', move, { passive: false })
    document.addEventListener('touchend', end, { passive: false })
    document.addEventListener('touchcancel', cancel)
    document.addEventListener('visibilitychange', visibility)
    window.addEventListener('blur', cancel)
    return {
        destroy() {
            finish()
            for (const { name, value, priority } of touchStyles) node.style.setProperty(name, value, priority)
            node.removeEventListener('touchstart', start)
            node.removeEventListener('dragstart', nativeDrag, true)
            node.removeEventListener('contextmenu', contextmenu, true)
            node.removeEventListener('click', click, true)
            document.removeEventListener('touchstart', multitouch, true)
            document.removeEventListener('touchmove', move)
            document.removeEventListener('touchend', end)
            document.removeEventListener('touchcancel', cancel)
            document.removeEventListener('visibilitychange', visibility)
            window.removeEventListener('blur', cancel)
        },
    }
}
