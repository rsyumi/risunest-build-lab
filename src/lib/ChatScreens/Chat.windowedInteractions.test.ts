// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { bootChatInteractionApp, interactionCharacterId, interactionConversationId } from './chatInteractionApp.testSupport'

let app: Awaited<ReturnType<typeof bootChatInteractionApp>> | undefined
let mounted: Record<string, any> | undefined
let target: HTMLDivElement

beforeEach(() => {
    target = document.createElement('div')
    target.style.height = '800px'
    document.body.append(target)
    vi.stubGlobal('ResizeObserver', class {
        observe() {}
        unobserve() {}
        disconnect() {}
    })
})

afterEach(async () => {
    if (mounted && app) await app.svelte.unmount(mounted)
    mounted = undefined
    app?.restore()
    app = undefined
    vi.restoreAllMocks()
    vi.useRealTimers()
    vi.unstubAllGlobals()
    document.body.replaceChildren()
})

async function mountSurface(surface: 'desktop' | 'mobile' | 'bookmark') {
    app = await bootChatInteractionApp()
    expect(app.runtime.getSelectedConversationMode()).toBe('windowed')
    const { default: Harness } = await import('./ChatInteractionSurfaces.test.svelte')
    mounted = app.svelte.mount(Harness, { target, props: { surface } })
    await app.svelte.tick()
    return app
}

function chatRow(index: number) { return target.querySelector(`.default-chat-screen [data-index="${index}"]`) }
function rows() { return [...target.querySelectorAll<HTMLElement>('.default-chat-screen [data-chat-probe]')] }
async function waitForLatest() {
    await vi.waitFor(() => {
        expect(chatRow(4999)).not.toBeNull()
        const containers = [...target.querySelectorAll<HTMLElement>('.default-chat-screen [data-chat-render-key]')]
        expect(containers.every((node) => node.children.length > 0)).toBe(true)
        expect(containers.some((node) => node.hasAttribute('data-chat-mount-pending'))).toBe(false)
    }, { timeout: 5000 })
    expect(rows().length).toBeLessThanOrEqual(64)
}
function folder(id: string) {
    const index = app!.selected().chatFolders!.findIndex((item) => item.id === id)
    const element = target.querySelector<HTMLElement>(`[data-risu-chat-folder-idx="${index}"]`)
    expect(element).not.toBeNull()
    return element!
}
function changeInput(input: HTMLInputElement, value: string) {
    input.value = value
    input.dispatchEvent(new Event('input', { bubbles: true }))
    input.dispatchEvent(new Event('change', { bubbles: true }))
}

it('navigates from a production persistent bookmark to its mounted distant viewport row', async () => {
    const current = await mountSurface('bookmark')
    await waitForLatest()
    expect(chatRow(123)).toBeNull()
    const capturedKinds: string[] = []
    const unsubscribe = current.stores.ScrollToMessageStore.subscribe((value) => {
        if (value) capturedKinds.push(value.kind)
    })
    const align = vi.spyOn(HTMLElement.prototype, 'scrollIntoView')
    try {
        current.stores.bookmarkListOpen.set(true)
        const navigationButton = () => target.querySelector<HTMLButtonElement>('[data-interaction-bookmarks] [role="button"] button[title]')
        await vi.waitFor(() => expect(navigationButton()).not.toBeNull())
        align.mockClear()
        navigationButton()!.click()
        await vi.waitFor(() => expect(chatRow(123)?.getAttribute('data-message'))
            .toBe('Synthetic interaction row 123'), { timeout: 5000 })
        await vi.waitFor(() => expect(align).toHaveBeenCalled())
        expect(align).toHaveBeenCalledWith({ behavior: 'instant', block: 'start' })
        expect(align.mock.contexts).toContain(chatRow(123)?.closest('[data-chat-render-key]'))
        expect(capturedKinds).toEqual(['persistent'])
        expect(get(current.stores.bookmarkListOpen)).toBe(false)
        expect(current.runtime.getSelectedConversationMode()).toBe('windowed')
        expect(current.completeLeases).not.toHaveBeenCalled()
        expect(current.fullReads).toEqual([])
        expect(rows().length).toBeLessThanOrEqual(64)
    } finally { unsubscribe() }
}, 60_000)

it.each(['desktop', 'mobile'] as const)('browses and edits the mounted %s chat list without a full-history lease', async (surface) => {
    const current = await mountSurface(surface)
    await waitForLatest()
    const originalRows = rows()
    if (surface === 'mobile') current.stores.MobileSideBar.set(1)
    else mounted!.showMenu(true)
    await vi.waitFor(() => expect(target.querySelector('[data-risu-chat-folder-idx]')).not.toBeNull())
    expect(current.completeLeases).not.toHaveBeenCalled()
    expect(current.fullReads).toEqual([])
    expect(current.runtime.getSelectedConversationMode()).toBe('windowed')

    if (surface === 'mobile') current.stores.MobileSideBar.set(0)
    else mounted!.showMenu(false)
    await current.svelte.tick()
    await waitForLatest()
    const retained = rows().filter((node) => originalRows.includes(node)).length
    if (surface === 'desktop') expect(retained).toBe(originalRows.length)
    else expect(retained).toBe(0) // MobileBody intentionally replaces the chat surface while its menu is open.

    if (surface === 'mobile') current.stores.MobileSideBar.set(1)
    else mounted!.showMenu(true)
    await vi.waitFor(() => expect(target.querySelector('[data-risu-chat-folder-idx]')).not.toBeNull())

    // Enter the real shared folder/chat name editor and commit both metadata fields.
    folder('folder-a').querySelectorAll<HTMLElement>('[role="button"]')[1].click()
    await current.svelte.tick()
    changeInput(folder('folder-a').querySelector<HTMLInputElement>('input')!, 'Renamed folder')
    await vi.waitFor(() => expect(current.selected().chatFolders![0].name).toBe('Renamed folder'))
    changeInput(target.querySelector<HTMLInputElement>('[data-risu-chat-idx="0"] input')!, 'Renamed conversation')
    await vi.waitFor(() => expect(current.selected().chats[0].name).toBe('Renamed conversation'))
    await current.runtime.flushPendingData('mounted-menu-edits')

    if (surface === 'mobile') current.stores.MobileSideBar.set(0)
    else mounted!.showMenu(false)
    await current.svelte.tick()
    await waitForLatest()
    const activeInstanceIds = current.chatMountProbe.mounts
        .filter(({ instanceId }) => !current.chatMountProbe.unmounts.includes(instanceId))
        .map(({ instanceId }) => instanceId).sort((left, right) => left - right)
    expect(activeInstanceIds).toEqual(rows().map((node) => Number(node.dataset.chatProbe)).sort((left, right) => left - right))
    expect(current.completeLeases).not.toHaveBeenCalled()
    expect(current.fullReads).toEqual([])
    expect(current.runtime.captureSelectedConversationAuthority()?.totalMessages).toBe(5000)
    const savedCharacter = await current.raw.readCharacter(interactionCharacterId)
    const savedConversation = await current.raw.readConversationMetadata(interactionCharacterId, interactionConversationId)
    expect(savedCharacter?.value.chatFolders?.[0].name).toBe('Renamed folder')
    expect(savedConversation?.value.conversation.name).toBe('Renamed conversation')
    expect(savedConversation?.value.totalMessages).toBe(5000)
}, 60_000)

it('applies delayed folder color and deletion by identity after persisted folder order changes', async () => {
    const current = await mountSurface('desktop')
    await waitForLatest()
    folder('folder-a').querySelectorAll<HTMLElement>('[role="button"]')[0].click()
    await vi.waitFor(() => expect(get(current.stores.alertStore).type).toBe('select'))
    current.stores.alertStore.set({ type: 'none', msg: '0' })
    await vi.waitFor(() => expect(get(current.stores.alertStore).type).toBe('select'))
    expect(await current.characters.editSelectedChatList(interactionCharacterId, 'synthetic-color-dialog-reorder', (owner) => {
        owner.chatFolders = [owner.chatFolders![1], owner.chatFolders![0]]
        return null
    })).toBe(true)
    current.stores.alertStore.set({ type: 'none', msg: '1' })
    await vi.waitFor(() => expect(current.selected().chatFolders!.find((item) => item.id === 'folder-a')?.color).toBe('green'))
    expect(current.selected().chatFolders!.find((item) => item.id === 'folder-b')?.color).not.toBe('green')
    await current.runtime.flushPendingData('mounted-folder-color')
    expect((await current.raw.readCharacter(interactionCharacterId))?.value.chatFolders?.find((item) => item.id === 'folder-a')?.color).toBe('green')

    folder('folder-a').querySelectorAll<HTMLElement>('[role="button"]')[2].click()
    await vi.waitFor(() => expect(get(current.stores.alertStore).type).toBe('ask'))
    expect(await current.characters.editSelectedChatList(interactionCharacterId, 'synthetic-concurrent-folder-order', (owner) => {
        owner.chatFolders = [owner.chatFolders![1], owner.chatFolders![0]]
        return null
    })).toBe(true)
    current.stores.alertStore.set({ type: 'none', msg: 'yes' })
    await vi.waitFor(() => expect(current.selected().chatFolders!.map((item) => item.id)).toEqual(['folder-b']))
    await current.runtime.flushPendingData('mounted-folder-delete')
    expect(current.selected().chats[0].folderId).toBeNull()
    expect((await current.raw.readCharacter(interactionCharacterId))?.value.chatFolders?.map((item) => item.id)).toEqual(['folder-b'])
    expect((await current.raw.readConversationMetadata(interactionCharacterId, interactionConversationId))?.value.conversation.folderId).toBeNull()
    expect(current.completeLeases).not.toHaveBeenCalled()
    expect(current.fullReads).toEqual([])
}, 60_000)
