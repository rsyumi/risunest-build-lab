import { get } from 'svelte/store'
import { alertStore } from '../stores.svelte'
import { isCompositionKey } from '../hotkeyModifier'

const key = 'risunestModal'
const live = new Set<string>()
let cleanupQueued = false
const historyTokens = (): string[] => Array.isArray(history.state?.[key]) ? history.state[key] : []
function scheduleCleanup() {
    if (cleanupQueued) return
    cleanupQueued = true
    queueMicrotask(() => {
        cleanupQueued = false
        const tokens = historyTokens()
        let count = 0
        for (let index = tokens.length - 1; index >= 0 && !live.has(tokens[index]); index--) count++
        if (count) history.go(-count)
    })
}
const alertVisible = () => !['none', 'toast', 'wait', 'progress'].includes(get(alertStore).type)
function cancelVisibleAlert(): boolean {
    // Escape or Back repeated before the next queued dialog shows stops here instead of closing the layer below.
    if (get(alertStore).dialogPending) return true
    if (!alertVisible()) return false
    alertStore.set({ type: 'none', msg: '' })
    return true
}

type NavigationOptions = {
    close(): void
    enabled?: boolean
    /** Leaves Escape to focused content, such as a code editor that closes its own widgets first. */
    leaveEscape?(event: KeyboardEvent): boolean
}
function navigationLayer(node: HTMLElement, initial: NavigationOptions, trapFocus: boolean) {
    let options = initial
    let token: string | null = null
    let opener: HTMLElement | null = null
    let lastFocused: HTMLElement | null = null
    const rememberFocus = (event: FocusEvent) => { if (event.target instanceof HTMLElement) lastFocused = event.target }
    node.addEventListener("focusin", rememberFocus)
    function activate() {
        if (token || options.enabled === false) return
        opener = document.activeElement instanceof HTMLElement ? document.activeElement : null
        token = crypto.randomUUID()
        live.add(token)
        history.pushState({ ...history.state, [key]: [...historyTokens(), token] }, '')
        queueMicrotask(() => {
            if (trapFocus && node.isConnected && isTopmost()) node.querySelector<HTMLElement>('button')?.focus()
        })
    }
    const isTopmost = () => token !== null && [...live].at(-1) === token
    const closeOnBack = () => {
        if (!token || historyTokens().includes(token)) return
        if (isTopmost() && cancelVisibleAlert()) {
            history.pushState({ ...history.state, [key]: [...historyTokens(), token] }, '')
            if (lastFocused?.isConnected) lastFocused.focus()
            return
        }
        options.close()
    }
    const keydown = (event: KeyboardEvent) => {
        if (!isTopmost() || isCompositionKey(event)) return
        if (event.key === 'Escape') {
            if (!alertVisible() && options.leaveEscape?.(event)) return
            event.preventDefault()
            event.stopImmediatePropagation()
            if (!cancelVisibleAlert()) options.close()
            else if (lastFocused?.isConnected) lastFocused.focus()
        }
        if (trapFocus && event.key === 'Tab' && ['none', 'toast', 'wait'].includes(get(alertStore).type)) {
            const items = [...node.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), a[href], [contenteditable]:not([contenteditable="false"]), [tabindex="0"]')].filter(item => item.getClientRects().length > 0)
            const first = items[0], last = items.at(-1)
            if (first && last && ((!node.contains(document.activeElement)) || (event.shiftKey && document.activeElement === first) || (!event.shiftKey && document.activeElement === last))) {
                event.preventDefault()
                ;(event.shiftKey ? last : first).focus()
            }
        }
    }
    function deactivate() {
        if (!token) return
        live.delete(token)
        token = null
        scheduleCleanup()
        // Focus the user already moved elsewhere, such as a text field tapped to close a menu, stays there.
        const focused = document.activeElement
        if (opener?.isConnected && (!focused || focused === document.body || node.contains(focused))) opener.focus()
    }
    window.addEventListener('popstate', closeOnBack)
    window.addEventListener('keydown', keydown, true)
    activate()
    return {
        update(value: NavigationOptions) {
            options = value
            if (options.enabled === false) deactivate()
            else activate()
        },
        destroy() {
            node.removeEventListener('focusin', rememberFocus)
            window.removeEventListener('popstate', closeOnBack)
            window.removeEventListener('keydown', keydown, true)
            deactivate()
        },
    }
}

/**
 * Android Back that the WebView cannot take itself. A dialog shown outside every layer is cancelled
 * instead of leaving the app, except RisuNest's own terms, where a refusal only reloads the same dialog.
 * A layer whose entry the WebView skips, because it was added without a tap, closes through the page.
 */
export function handleRootBack(event: Event) {
    if (get(alertStore).type === 'tos') return
    if (cancelVisibleAlert()) event.preventDefault()
    else if (historyTokens().some(token => live.has(token))) {
        event.preventDefault()
        history.back()
    }
}

/** Menus participate in Back and Escape without trapping keyboard focus. */
export function backNavigationLayer(node: HTMLElement, options: NavigationOptions) {
    return navigationLayer(node, options, false)
}

/** Let browser/WebView Back dismiss a local dialog and restore its opener. */
export function modalNavigation(node: HTMLElement, options: NavigationOptions) {
    return navigationLayer(node, options, true)
}
