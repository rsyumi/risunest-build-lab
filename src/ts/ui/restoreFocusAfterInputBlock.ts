import { tick } from 'svelte'
import type { Readable } from 'svelte/store'

/**
 * An inert page moves focus to the body and never returns it. Refocus the control
 * that held focus when input was blocked, unless the user has focused something else.
 */
export function restoreFocusAfterInputBlock(blocked: Readable<boolean>): () => void {
    let focused: HTMLElement | null = null
    let generation = 0
    return blocked.subscribe((value) => {
        const current = ++generation
        if (value) {
            const active = document.activeElement
            if (!focused && active instanceof HTMLElement && active !== document.body) {
                focused = active
            }
            return
        }
        const target = focused
        if (!target) return
        void tick().then(() => {
            // A block that started meanwhile keeps the same target for its own release.
            if (current !== generation) return
            focused = null
            if (!target.isConnected || target.closest('[inert]')) return
            const active = document.activeElement
            if (active && active !== document.body && active !== document.documentElement) return
            target.focus({ preventScroll: true })
        })
    })
}
