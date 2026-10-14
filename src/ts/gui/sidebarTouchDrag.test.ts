import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { sidebarTouchDrag } from './sidebarTouchDrag'
import { RISU_SIDEBAR_DRAG_TYPE } from '../dragTypes'

let node: HTMLElement
let source: HTMLElement
let avatar: HTMLElement
let target: HTMLElement
let action: ReturnType<typeof sidebarTouchDrag>
let started: ReturnType<typeof vi.fn<(event: DragEvent) => void>>
let dropped: ReturnType<typeof vi.fn<(event: DragEvent) => void>>
let ended: ReturnType<typeof vi.fn<(event: DragEvent) => void>>
let hit: Element | null

function touch(type: string, x = 30, y = 60, count = 1) {
    const point = { identifier: 7, clientX: x, clientY: y, target: avatar }
    const event = new Event(type, { bubbles: true, cancelable: true })
    Object.defineProperties(event, {
        touches: { value: type === 'touchend' || type === 'touchcancel' ? [] : Array.from({ length: count }, (_, i) => ({ ...point, identifier: 7 + i })) },
        changedTouches: { value: [point] },
    })
    avatar.dispatchEvent(event)
    return event
}

beforeEach(() => {
    vi.useFakeTimers()
    // happy-dom's DragEvent does not implement DragEventInit yet.
    vi.stubGlobal('DragEvent', class extends MouseEvent {
        dataTransfer: DataTransfer | null
        constructor(type: string, init: DragEventInit) {
            super(type, init)
            this.dataTransfer = init.dataTransfer ?? null
        }
    })
    node = document.createElement('div')
    node.innerHTML = '<div draggable="true" data-sidebar-drag-id="one" data-sidebar-drop><span class="avatar">One</span></div><div data-sidebar-drop></div>'
    document.body.append(node)
    source = node.children[0] as HTMLElement
    avatar = source.children[0] as HTMLElement
    target = node.children[1] as HTMLElement
    hit = target
    vi.spyOn(node, 'getClientRects').mockReturnValue([{}] as unknown as DOMRectList)
    vi.spyOn(node, 'getBoundingClientRect').mockReturnValue({ top: 0, bottom: 300, left: 0, right: 80, height: 300 } as DOMRect)
    vi.spyOn(document, 'elementFromPoint').mockImplementation(() => hit)
    started = vi.fn((event: DragEvent) => event.dataTransfer!.setData(RISU_SIDEBAR_DRAG_TYPE, 'true'))
    dropped = vi.fn()
    ended = vi.fn()
    source.addEventListener('dragstart', started)
    source.addEventListener('dragend', ended)
    target.addEventListener('dragover', event => {
        if (event.dataTransfer!.types.includes(RISU_SIDEBAR_DRAG_TYPE)) event.preventDefault()
    })
    target.addEventListener('drop', dropped)
    action = sidebarTouchDrag(node)
})

afterEach(() => {
    action.destroy()
    document.body.replaceChildren()
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
    vi.useRealTimers()
})

it('leaves a short tap clickable and excludes the legacy touch polyfill', () => {
    const legacy = vi.fn()
    document.addEventListener('touchstart', legacy)
    try {
        expect(touch('touchstart').defaultPrevented).toBe(false)
        vi.advanceTimersByTime(100)
        expect(touch('touchend').defaultPrevented).toBe(false)
        const click = new MouseEvent('click', { bubbles: true, cancelable: true, detail: 1 })
        avatar.dispatchEvent(click)
        expect(click.defaultPrevented).toBe(false)
        expect(legacy).not.toHaveBeenCalled()
        expect(started).not.toHaveBeenCalled()
    } finally { document.removeEventListener('touchstart', legacy) }
})

it('preserves scrolling started before the hold delay, including a later pause', () => {
    touch('touchstart')
    vi.advanceTimersByTime(80)
    expect(touch('touchmove', 30, 30).defaultPrevented).toBe(false)
    vi.advanceTimersByTime(700)
    expect(touch('touchmove', 30, 160).defaultPrevented).toBe(false)
    touch('touchend', 30, 160)
    expect(started).not.toHaveBeenCalled()
    expect(dropped).not.toHaveBeenCalled()
})

it('tolerates finger jitter, then drags and drops after holding', () => {
    touch('touchstart')
    touch('touchmove', 32, 62)
    vi.advanceTimersByTime(450)
    expect(touch('touchmove', 33, 63).defaultPrevented).toBe(true)
    expect(document.querySelector('[data-sidebar-drag-ghost]')).toBeNull()
    expect(touch('touchmove', 30, 160).defaultPrevented).toBe(true)
    expect(started).toHaveBeenCalledOnce()
    expect(document.querySelector('[data-sidebar-drag-ghost]')).not.toBeNull()
    expect(touch('touchend', 30, 170).defaultPrevented).toBe(true)
    expect(dropped).toHaveBeenCalledOnce()
    expect(dropped.mock.calls[0][0].clientY).toBe(170)
    expect(ended).toHaveBeenCalledOnce()
    expect(document.querySelector('[data-sidebar-drag-ghost]')).toBeNull()
    const click = new MouseEvent('click', { bubbles: true, cancelable: true, detail: 1 })
    avatar.dispatchEvent(click)
    expect(click.defaultPrevented).toBe(true)
})

it('opens the existing context menu only on release of a stationary hold', () => {
    const menu = vi.fn()
    avatar.addEventListener('contextmenu', menu)
    touch('touchstart')
    vi.advanceTimersByTime(500)
    const nativeMenu = new MouseEvent('contextmenu', { bubbles: true, cancelable: true })
    avatar.dispatchEvent(nativeMenu)
    expect(nativeMenu.defaultPrevented).toBe(true)
    expect(menu).not.toHaveBeenCalled()
    touch('touchend')
    expect(menu).toHaveBeenCalledOnce()
    expect(started).not.toHaveBeenCalled()
})

it.each(['touchcancel', 'multitouch', 'blur', 'destroy', 'replaced'])('cancels an active drag on %s without dropping', reason => {
    touch('touchstart')
    vi.advanceTimersByTime(450)
    touch('touchmove', 30, 160)
    if (reason === 'touchcancel') touch('touchcancel')
    if (reason === 'multitouch') touch('touchstart', 30, 160, 2)
    if (reason === 'blur') window.dispatchEvent(new Event('blur'))
    if (reason === 'destroy') action.destroy()
    if (reason === 'replaced') {
        source.dataset.sidebarDragId = 'different'
        vi.advanceTimersByTime(20)
    }
    touch('touchend', 30, 170)
    expect(dropped).not.toHaveBeenCalled()
    expect(ended).toHaveBeenCalledOnce()
    expect(document.querySelector('[data-sidebar-drag-ghost]')).toBeNull()
})

it('does not drop outside the list and scrolls near its edge during a drag', () => {
    touch('touchstart')
    vi.advanceTimersByTime(450)
    touch('touchmove', 30, 290)
    vi.advanceTimersByTime(100)
    expect(node.scrollTop).toBeGreaterThan(0)
    hit = document.body
    touch('touchend', 150, 290)
    expect(dropped).not.toHaveBeenCalled()
    expect(ended).toHaveBeenCalledOnce()
})

it('does not intercept mouse dragging, right click or keyboard activation', () => {
    const drag = new DragEvent('dragstart', { bubbles: true, cancelable: true, dataTransfer: new DataTransfer() })
    source.dispatchEvent(drag)
    expect(drag.defaultPrevented).toBe(false)
    const menu = new MouseEvent('contextmenu', { bubbles: true, cancelable: true })
    avatar.dispatchEvent(menu)
    expect(menu.defaultPrevented).toBe(false)
    const click = new MouseEvent('click', { bubbles: true, cancelable: true, detail: 0 })
    avatar.dispatchEvent(click)
    expect(click.defaultPrevented).toBe(false)
})

it('restores the previous text-selection style when detached', () => {
    action.destroy()
    node.style.setProperty('user-select', 'text', 'important')
    action = sidebarTouchDrag(node)
    expect(node.style.getPropertyValue('user-select')).toBe('none')
    action.destroy()
    expect(node.style.getPropertyValue('user-select')).toBe('text')
    expect(node.style.getPropertyPriority('user-select')).toBe('important')
})
