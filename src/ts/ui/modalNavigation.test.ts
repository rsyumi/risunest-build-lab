import { afterEach, expect, it, vi } from 'vitest'
vi.mock('../stores.svelte', async () => ({ alertStore: (await import('svelte/store')).writable({ type: 'none', msg: '' }) }))
import { alertStore } from '../stores.svelte'
import { get } from 'svelte/store'
import { modalNavigation, backNavigationLayer } from './modalNavigation'

afterEach(() => {
    alertStore.set({ type: "none", msg: "" })
    vi.restoreAllMocks()
    history.replaceState(null, '')
    document.body.replaceChildren()
})
it('dismisses on Escape and restores focus to the opener', async () => {
    const opener = document.createElement('button'),
        modal = document.createElement('div')
    modal.innerHTML = '<button>Close</button>'
    document.body.append(opener, modal)
    opener.focus()
    const back = vi.spyOn(history, 'go').mockImplementation(() => {})
    const close = vi.fn()
    const action = modalNavigation(modal, { close })
    await Promise.resolve()
    expect(document.activeElement).toBe(modal.firstChild)
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    expect(close).toHaveBeenCalledOnce()
    action.destroy()
    expect(document.activeElement).toBe(opener)
    await Promise.resolve()
    expect(back).toHaveBeenCalledWith(-1)
    back.mockRestore()
    history.replaceState(null, '')
    document.body.replaceChildren()
})
it('consumes a WebView/browser Back without going back a second time', () => {
    const back = vi.spyOn(history, 'go').mockImplementation(() => {})
    const close = vi.fn(),
        modal = document.createElement('div')
    const action = modalNavigation(modal, { close })
    history.replaceState(null, '')
    window.dispatchEvent(new PopStateEvent('popstate'))
    expect(close).toHaveBeenCalledOnce()
    action.destroy()
    expect(back).not.toHaveBeenCalled()
    back.mockRestore()
})

it('dismisses only the top nested dialog on Escape and preserves ancestors on Back', () => {
    const nodes = Array.from({ length: 3 }, () => document.createElement('div'))
    const closes = nodes.map(() => vi.fn())
    const states: unknown[] = []
    const actions = nodes.map((node, index) => {
        const action = modalNavigation(node, { close: closes[index] })
        states.push(history.state)
        return action
    })
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    expect(closes[0]).not.toHaveBeenCalled()
    expect(closes[1]).not.toHaveBeenCalled()
    expect(closes[2]).toHaveBeenCalledOnce()

    history.replaceState(states[1], '')
    window.dispatchEvent(new PopStateEvent('popstate'))
    actions[2].destroy()
    expect(closes[0]).not.toHaveBeenCalled()
    expect(closes[1]).not.toHaveBeenCalled()

    history.replaceState(states[0], '')
    window.dispatchEvent(new PopStateEvent('popstate'))
    actions[1].destroy()
    expect(closes[0]).not.toHaveBeenCalled()
    expect(closes[1]).toHaveBeenCalledOnce()

    history.replaceState(null, '')
    window.dispatchEvent(new PopStateEvent('popstate'))
    actions[0].destroy()
    expect(closes[0]).toHaveBeenCalledOnce()
})

it('does not close a dialog for composition-owned Escape', () => {
    const close = vi.fn()
    const action = modalNavigation(document.createElement('div'), { close })
    for (const flags of [{ isComposing: true }, { keyCode: 229 }]) {
        const event = new KeyboardEvent('keydown', { key: 'Escape', cancelable: true, ...flags })
        window.dispatchEvent(event)
        expect(event.defaultPrevented).toBe(false)
    }
    expect(close).not.toHaveBeenCalled()
    history.replaceState(null, '')
    action.destroy()
})

it('coalesces nested teardown into one asynchronous traversal', async () => {
    const go = vi.spyOn(history, 'go').mockImplementation(() => {})
    const parent = modalNavigation(document.createElement('div'), { close: vi.fn() })
    const child = modalNavigation(document.createElement('div'), { close: vi.fn() })
    child.destroy()
    parent.destroy()
    await Promise.resolve()
    expect(go).toHaveBeenCalledOnce()
    expect(go).toHaveBeenCalledWith(-2)
})
it('cancels a sibling confirmation before closing a modal for Escape and Back', () => {
    const close = vi.fn()
    const action = modalNavigation(document.createElement('div'), { close })
    alertStore.set({ type: 'ask', msg: 'confirm' })
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    expect(get(alertStore)).toEqual({ type: 'none', msg: '' })
    expect(close).not.toHaveBeenCalled()
    alertStore.set({ type: 'ask', msg: 'confirm again' })
    history.replaceState(null, '')
    window.dispatchEvent(new PopStateEvent('popstate'))
    expect(get(alertStore)).toEqual({ type: 'none', msg: '' })
    expect(history.state.risunestModal).toHaveLength(1)
    expect(close).not.toHaveBeenCalled()
    history.replaceState(null, '')
    action.destroy()
})
it('lets menu layers keep focus and opt out without a history entry', async () => {
    const opener = document.createElement('button'), menu = document.createElement('div')
    menu.innerHTML = '<button>Item</button>'
    document.body.append(opener, menu)
    opener.focus()
    const action = backNavigationLayer(menu, { close: vi.fn(), enabled: false })
    expect(history.state).toBeNull()
    action.update({ close: vi.fn(), enabled: true })
    await Promise.resolve()
    expect(document.activeElement).toBe(opener)
    history.replaceState(null, '')
    action.destroy()
})
it('leaves Escape to content that asks for it without closing the modal below', () => {
    vi.spyOn(history, 'go').mockImplementation(() => {})
    const lower = document.createElement('div'), upper = document.createElement('div')
    upper.innerHTML = '<div data-editor><textarea></textarea></div><button>Close</button>'
    document.body.append(lower, upper)
    const editor = upper.querySelector('[data-editor]')!, field = upper.querySelector('textarea')!
    const closeLower = vi.fn(), closeUpper = vi.fn()
    const lowerAction = modalNavigation(lower, { close: closeLower })
    const upperAction = modalNavigation(upper, { close: closeUpper, leaveEscape: (event) => editor.contains(event.target as Node) })
    const escape = () => new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })

    const left = escape()
    field.dispatchEvent(left)
    expect(left.defaultPrevented).toBe(false)
    expect(closeUpper).not.toHaveBeenCalled()
    expect(closeLower).not.toHaveBeenCalled()

    alertStore.set({ type: 'ask', msg: 'confirm' })
    const overAlert = escape()
    field.dispatchEvent(overAlert)
    expect(overAlert.defaultPrevented).toBe(true)
    expect(get(alertStore)).toEqual({ type: 'none', msg: '' })

    upper.querySelector('button')!.dispatchEvent(escape())
    expect(closeUpper).toHaveBeenCalledOnce()
    expect(closeLower).not.toHaveBeenCalled()
    upperAction.destroy()
    lowerAction.destroy()
})
