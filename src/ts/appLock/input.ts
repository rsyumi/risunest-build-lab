import { tick } from 'svelte'

export interface AppLockSurface {
    element: HTMLElement
    keydown: (event: KeyboardEvent) => void
}

let blocked = false
let gate: AppLockSurface | null = null
let exit: AppLockSurface | null = null
let previousFocus: HTMLElement | null = null
let focusGeneration = 0

export const isAppLockInputBlocked = (): boolean => blocked
export function setAppLockInputBlocked(value: boolean): void {
    if (blocked === value) return
    blocked = value
    const generation = ++focusGeneration
    if (value) {
        previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null
        return
    }
    const target = previousFocus
    const oldGate = gate?.element
    const oldExit = exit?.element
    previousFocus = null
    void tick().then(() => {
        if (generation !== focusGeneration || blocked || !target?.isConnected || target.closest('[inert]')) return
        const current = document.activeElement
        if (current === document.body || current === document.documentElement
            || oldGate?.contains(current) || oldExit?.contains(current)) target.focus({ preventScroll: true })
    })
}
export function registerAppLockSurface(surface: AppLockSurface | null): void { gate = surface }
export function registerAppLockExitSurface(surface: AppLockSurface | null): void {
    const previous = exit
    exit = surface
    if (!surface && previous) void tick().then(() => {
        if (!blocked || exit || !gate?.element.isConnected) return
        const current = document.activeElement
        if (current === document.body || current === document.documentElement || previous.element.contains(current)) {
            gate.element.focus({ preventScroll: true })
        }
    })
}

export function handleAppLockSurfaceNavigation(event: KeyboardEvent, root: HTMLElement): void {
    if (event.key === 'Tab') {
        const buttons = [...root.querySelectorAll<HTMLElement>('button:not(:disabled), a[href], summary, [tabindex="0"]')]
            .filter(item => {
                const closedDetails = item.closest('details:not([open])')
                return !item.closest('[inert], [hidden]') && item.getClientRects().length > 0
                    && (!closedDetails || closedDetails.querySelector(':scope > summary')?.contains(item))
            })
        event.preventDefault()
        if (!buttons.length) { root.focus(); return }
        const index = buttons.indexOf(document.activeElement as HTMLElement)
        const next = index < 0 ? (event.shiftKey ? buttons.length - 1 : 0)
            : (index + (event.shiftKey ? -1 : 1) + buttons.length) % buttons.length
        buttons[next].focus()
    } else if ((event.key === 'Enter' || event.key === ' ') && !event.repeat) {
        const focused = document.activeElement
        if (focused instanceof HTMLElement && root.contains(focused)
            && ((focused instanceof HTMLButtonElement && !focused.disabled) || focused.matches('summary'))) {
            event.preventDefault()
            focused.click()
        }
    }
}

export function installAppLockInput(): () => void {
    const active = () => exit?.element.isConnected ? exit : gate?.element.isConnected ? gate : null
    const keydown = (event: KeyboardEvent) => {
        if (!blocked) return
        const surface = active()
        if (surface) surface.keydown(event)
        else event.preventDefault()
        if (event.key === 'Escape') event.preventDefault()
        event.stopImmediatePropagation()
    }
    const keyup = (event: KeyboardEvent) => { if (blocked) event.stopImmediatePropagation() }
    const focusin = (event: FocusEvent) => {
        if (!blocked) return
        const surface = active()
        if (surface && !surface.element.contains(event.target as Node)) {
            event.stopImmediatePropagation()
            surface.element.focus()
        }
    }
    window.addEventListener('keydown', keydown, true)
    window.addEventListener('keyup', keyup, true)
    window.addEventListener('focusin', focusin, true)
    return () => {
        window.removeEventListener('keydown', keydown, true)
        window.removeEventListener('keyup', keyup, true)
        window.removeEventListener('focusin', focusin, true)
    }
}
