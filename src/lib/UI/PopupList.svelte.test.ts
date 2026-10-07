// @vitest-environment happy-dom

import { afterEach, expect, it, vi } from 'vitest'
import { createRawSnippet, flushSync, mount, unmount } from 'svelte'
import { popupStore } from 'src/ts/stores.svelte'
import PopupList from './PopupList.svelte'

vi.mock('src/ts/stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    const popupStore = $state({ children: null as null | import('svelte').Snippet, mouseX: 0, mouseY: 0, openId: 0 })
    return { popupStore, alertStore: writable({ type: 'none', msg: '' }) }
})
vi.mock('src/ts/util', () => ({ sleep: () => Promise.resolve() }))

const menu = createRawSnippet(() => ({ render: () => '<button>Synthetic menu item</button>' }))
let mounted: ReturnType<typeof mount> | undefined

function openMenu(openId: number) {
    const opener = document.createElement('button')
    document.body.append(opener)
    opener.focus()
    popupStore.children = menu
    popupStore.openId = openId
    const target = document.createElement('div')
    document.body.append(target)
    mounted = mount(PopupList, { target })
    flushSync()
    return target
}

afterEach(async () => {
    if (mounted) await unmount(mounted)
    mounted = undefined
    popupStore.children = null
    popupStore.openId = 0
    vi.restoreAllMocks()
    history.replaceState(null, '')
    document.body.replaceChildren()
})

it('closes the message menu on Back and lets its button open it again', () => {
    const target = openMenu(7)
    expect(target.textContent).toContain('Synthetic menu item')
    expect(history.state?.risunestModal).toHaveLength(1)

    history.replaceState(null, '')
    window.dispatchEvent(new PopStateEvent('popstate'))
    flushSync()

    expect(popupStore.children).toBeNull()
    expect(popupStore.openId).toBe(0)
    expect(target.textContent).not.toContain('Synthetic menu item')
})

it('closes the message menu on Escape and removes its history entry', async () => {
    const go = vi.spyOn(history, 'go').mockImplementation(() => {})
    const target = openMenu(7)
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', cancelable: true }))
    flushSync()
    expect(popupStore.children).toBeNull()
    expect(target.textContent).not.toContain('Synthetic menu item')
    await Promise.resolve()
    expect(go).toHaveBeenCalledWith(-1)
})

it('leaves focus in a text field tapped while the menu is open', async () => {
    vi.spyOn(history, 'go').mockImplementation(() => {})
    const field = document.createElement('textarea')
    document.body.append(field)
    const target = openMenu(7)
    const opener = document.activeElement
    expect(opener).toBeInstanceOf(HTMLButtonElement)
    await Promise.resolve()
    field.focus()
    field.click()
    flushSync()
    expect(target.textContent).not.toContain('Synthetic menu item')
    expect(document.activeElement).toBe(field)
})

it('returns focus to the menu button when a menu item closes the menu', async () => {
    vi.spyOn(history, 'go').mockImplementation(() => {})
    const target = openMenu(7)
    const opener = document.activeElement
    await Promise.resolve()
    const item = target.querySelector('button')!
    item.focus()
    item.click()
    flushSync()
    expect(target.textContent).not.toContain('Synthetic menu item')
    expect(document.activeElement).toBe(opener)
})
