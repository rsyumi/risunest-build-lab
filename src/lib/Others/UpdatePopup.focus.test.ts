import { afterEach, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import { appUpdateState, initialAppUpdateState } from 'src/ts/update/state.svelte'
import type { AvailableAppUpdate } from 'src/ts/update/manifest'

const mocks = vi.hoisted(() => ({ cancel: vi.fn() }))
vi.mock('src/ts/update/controller', () => ({ applyAppUpdate: vi.fn(), cancelAppUpdateDownload: mocks.cancel, dismissAppUpdate: vi.fn(), skipAppUpdate: vi.fn() }))
vi.mock('src/ts/storage/database.svelte', () => ({ getDatabase: () => ({ language: 'en' }) }))
vi.mock('src/ts/globalApi.svelte', () => ({ openURL: vi.fn() }))
vi.mock('@tauri-apps/plugin-process', () => ({ exit: vi.fn() }))
vi.mock('src/lang', () => ({ language: { risuNest: { update: { dialogTitle: 'Update', downloading: 'Downloading', cancel: 'Cancel' } } } }))
import UpdatePopup from './UpdatePopup.svelte'

const update: AvailableAppUpdate = {
    handleId: 'synthetic', version: '2.0.0', pubDate: '2026-09-28T00:00:00Z', notes: '', localizedNotes: {},
    releasePage: '', installStrategy: 'self-install', downloadUrl: '', downloadSize: 100, format: 'nsis',
}
let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement | undefined
afterEach(async () => {
    if (component) await unmount(component)
    target?.remove()
    appUpdateState.set({ ...initialAppUpdateState })
    vi.clearAllMocks()
})

it('retains Cancel focus through progress and focuses the panel again on reopen', () => {
    target = document.createElement('div')
    document.body.append(target)
    appUpdateState.set({ ...initialAppUpdateState, popupVisible: true, phase: 'downloading', update })
    component = mount(UpdatePopup, { target })
    flushSync()
    expect(document.activeElement).toBe(target.querySelector('[role=dialog]'))
    const cancel = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === 'Cancel')!
    cancel.focus()
    for (const downloaded of [10, 20, 30]) {
        appUpdateState.update(value => ({ ...value, progress: { handleId: 'synthetic', downloaded, total: 100 } }))
        flushSync()
        expect(document.activeElement).toBe(cancel)
    }
    cancel.click()
    expect(mocks.cancel).toHaveBeenCalledOnce()
    appUpdateState.update(value => ({ ...value, popupVisible: false }))
    flushSync()
    appUpdateState.update(value => ({ ...value, popupVisible: true }))
    flushSync()
    expect(document.activeElement).toBe(target.querySelector('[role=dialog]'))
})

