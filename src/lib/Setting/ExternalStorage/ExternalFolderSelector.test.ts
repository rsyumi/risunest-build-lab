// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const state = vi.hoisted(() => ({
    listFolders: vi.fn(),
    selectFolder: vi.fn(),
    cancelFolderSelection: vi.fn(),
}))

vi.mock('src/ts/storage/sync/external/bridge', () => ({
    getExternalStorageBridge: () => state,
}))

import ExternalFolderSelector from './ExternalFolderSelector.svelte'
import { externalStorageStrings } from './strings'

let target: HTMLDivElement
let component: ReturnType<typeof mount> | undefined
const strings = externalStorageStrings('en')

async function settle(): Promise<void> {
    await tick()
    await Promise.resolve()
    await tick()
}

function button(text: string): HTMLButtonElement {
    const result = [...target.querySelectorAll<HTMLButtonElement>('button')]
        .find(item => item.textContent?.trim() === text)
    if (!result) throw new Error(`Missing button: ${text}`)
    return result
}

function mountSelector(props: Record<string, unknown> = {}): void {
    component = mount(ExternalFolderSelector, {
        target,
        props: {
            strings,
            selectionId: 'selection-1',
            onselected: vi.fn(),
            oncancel: vi.fn(),
            ...props,
        },
    })
}

beforeEach(() => {
    for (const mock of Object.values(state)) mock.mockReset()
    target = document.createElement('div')
    document.body.append(target)
    state.cancelFolderSelection.mockResolvedValue(undefined)
    state.selectFolder.mockResolvedValue({ name: 'Documents' })
    state.listFolders.mockResolvedValue({
        path: [],
        folders: [{ name: 'Documents', handle: 'h-docs' }],
        selectable: false,
    })
})

afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    target.remove()
})

describe('ExternalFolderSelector', () => {
    it('loads the root, focuses the title and disables root selection', async () => {
        mountSelector()
        await settle()
        expect(state.listFolders).toHaveBeenCalledWith({ selectionId: 'selection-1' })
        expect(document.activeElement?.id).toBe('external-folder-selector-title')
        expect(button(strings.selectThisFolder).disabled).toBe(true)
        expect(target.querySelectorAll('.crumb')).toHaveLength(1)
    })

    it('navigates into a folder and back through the breadcrumb', async () => {
        state.listFolders.mockImplementation(async (request: { folder?: string }) => request.folder
            ? {
                path: [{ name: 'Documents', handle: 'h-docs' }],
                folders: [{ name: 'Backups', handle: 'h-backups' }],
                selectable: true,
            }
            : { path: [], folders: [{ name: 'Documents', handle: 'h-docs' }], selectable: false })
        mountSelector()
        await settle()
        button('Documents').click()
        await settle()
        expect(state.listFolders).toHaveBeenLastCalledWith({ selectionId: 'selection-1', folder: 'h-docs' })
        expect(button('Documents').getAttribute('aria-current')).toBe('location')
        expect(button(strings.selectThisFolder).disabled).toBe(false)
        button('OneDrive').click()
        await settle()
        expect(state.listFolders).toHaveBeenLastCalledWith({ selectionId: 'selection-1' })
    })

    it('appends a next page without exposing handles or cursors', async () => {
        state.listFolders.mockImplementation(async (request: { cursor?: string }) => request.cursor
            ? {
                path: [{ name: 'Documents', handle: 'h-docs' }],
                folders: [{ name: 'Second', handle: 'h-second', size: 42 }],
                selectable: true,
            }
            : {
                path: [{ name: 'Documents', handle: 'h-docs' }],
                folders: [{ name: 'First', handle: 'h-first' }],
                nextCursor: 'cursor-secret',
                selectable: true,
            })
        mountSelector()
        await settle()
        const list = target.querySelector<HTMLUListElement>('.list')!
        Object.defineProperties(list, {
            scrollTop: { value: 100, configurable: true },
            clientHeight: { value: 100, configurable: true },
            scrollHeight: { value: 200, configurable: true },
        })
        list.dispatchEvent(new Event('scroll'))
        await settle()
        expect(state.listFolders).toHaveBeenLastCalledWith({
            selectionId: 'selection-1', folder: 'h-docs', cursor: 'cursor-secret',
        })
        expect(target.textContent).toContain('First')
        expect(target.textContent).toContain('Second')
        expect(target.textContent).not.toContain('cursor-secret')
        expect(target.textContent).not.toContain('h-second')
        expect(target.textContent).not.toContain('42')
    })

    it('keeps loaded folders and retries a failed next page', async () => {
        state.listFolders
            .mockResolvedValueOnce({ path: [], folders: [{ name: 'First', handle: 'h-first' }], nextCursor: 'next', selectable: false })
            .mockRejectedValueOnce({ kind: 'transient' })
            .mockResolvedValueOnce({ path: [], folders: [{ name: 'Second', handle: 'h-second' }], selectable: false })
        mountSelector()
        await settle()
        const list = target.querySelector<HTMLUListElement>('.list')!
        Object.defineProperties(list, {
            scrollTop: { value: 100, configurable: true },
            clientHeight: { value: 100, configurable: true },
            scrollHeight: { value: 200, configurable: true },
        })
        list.dispatchEvent(new Event('scroll'))
        await settle()
        expect(target.textContent).toContain('First')
        expect(target.querySelector('[role="alert"]')?.textContent).toBe(strings.retry)
        button(strings.retryAction).click()
        await settle()
        expect(state.listFolders).toHaveBeenLastCalledWith({ selectionId: 'selection-1', cursor: 'next' })
        expect(target.textContent).toContain('Second')
    })

    it('drops a page that arrives after navigation moved elsewhere', async () => {
        let resolveOld!: (value: unknown) => void
        state.listFolders.mockImplementation(async (request: { folder?: string }) => {
            if (request.folder === 'h-old') return new Promise(resolve => { resolveOld = resolve })
            if (request.folder === 'h-new') return {
                path: [{ name: 'New', handle: 'h-new' }],
                folders: [{ name: 'Current child', handle: 'h-current' }],
                selectable: true,
            }
            return {
                path: [],
                folders: [{ name: 'Old', handle: 'h-old' }, { name: 'New', handle: 'h-new' }],
                selectable: false,
            }
        })
        mountSelector()
        await settle()
        button('Old').click()
        await tick()
        button('OneDrive').click()
        await settle()
        button('New').click()
        await settle()
        resolveOld({
            path: [{ name: 'Old', handle: 'h-old' }],
            folders: [{ name: 'Stale child', handle: 'h-stale' }],
            selectable: true,
        })
        await settle()
        expect(target.textContent).toContain('Current child')
        expect(target.textContent).not.toContain('Stale child')
    })

    it('selects the current folder through the native session', async () => {
        const onselected = vi.fn()
        state.listFolders.mockResolvedValue({
            path: [{ name: 'Documents', handle: 'h-docs' }], folders: [], selectable: true,
        })
        mountSelector({ onselected })
        await settle()
        button(strings.selectThisFolder).click()
        await settle()
        expect(state.selectFolder).toHaveBeenCalledWith({ selectionId: 'selection-1', folder: 'h-docs' })
        expect(onselected).toHaveBeenCalledWith({ name: 'Documents' })
    })

    it('keeps navigation usable after native selection rejection', async () => {
        state.listFolders.mockResolvedValue({
            path: [{ name: 'Documents', handle: 'h-docs' }],
            folders: [{ name: 'Child', handle: 'h-child' }],
            selectable: true,
        })
        state.selectFolder.mockRejectedValue({ kind: 'preconditionFailed' })
        mountSelector()
        await settle()
        button(strings.selectThisFolder).click()
        await settle()
        expect(target.querySelector('[role="alert"]')?.textContent).toBe(strings.stateChanged)
        expect(button('Child').disabled).toBe(false)
    })

    it('shows the empty message for a folder without subfolders', async () => {
        state.listFolders.mockResolvedValue({
            path: [{ name: 'Empty', handle: 'h-empty' }], folders: [], selectable: true,
        })
        mountSelector()
        await settle()
        expect(target.textContent).toContain(strings.noSubfolders)
    })

    it.each([
        ['Escape', () => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))],
        ['close', () => target.querySelector<HTMLButtonElement>(`button[aria-label="${strings.close}"]`)!.click()],
        ['Cancel', () => button(strings.cancel).click()],
    ])('cancels the session once through %s', async (_name, close) => {
        const oncancel = vi.fn()
        mountSelector({ oncancel })
        await settle()
        close()
        close()
        await settle()
        expect(state.cancelFolderSelection).toHaveBeenCalledExactlyOnceWith('selection-1')
        expect(oncancel).toHaveBeenCalledTimes(1)
    })
})
