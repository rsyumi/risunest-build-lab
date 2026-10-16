vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule())
vi.mock('src/ts/storage/persistentDataRuntime.svelte', async importOriginal => ({
    ...await importOriginal<typeof import('src/ts/storage/persistentDataRuntime.svelte')>(),
    persistentLocalSaveFailure: (await import('svelte/store')).writable<unknown | null>(null),
}))
import { afterEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import type { writable } from 'svelte/store'
import { language } from 'src/lang'
import { DBState, MobileGUI, sideBarStore } from 'src/ts/stores.svelte'
import { persistentLocalSaveFailure } from 'src/ts/storage/persistentDataRuntime.svelte'
import { syncStatus, type SyncStatusSource } from 'src/ts/storage/sync/syncStatus'
import type { ServerSyncView } from 'src/ts/storage/sync/syncStatusView'
import SideBarArrow from './SideBarArrow.svelte'

const saveFailure = persistentLocalSaveFailure as ReturnType<typeof writable<unknown | null>>
let component: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    document.body.replaceChildren()
    syncStatus.set(null)
    saveFailure.set(null)
    DBState.db.risunestSyncStatusUi = true
})
const server = (view: Partial<ServerSyncView>): SyncStatusSource => ({
    target: 'server',
    view: { status: { configured: true, bound: true }, running: false, paused: false, error: '', ...view } as ServerSyncView,
    pendingChanges: async () => 0,
    syncNow: async () => {},
})

it('marks the closed sidebar tab with every sync state except connected', async () => {
    MobileGUI.set(false)
    sideBarStore.set(false)
    component = mount(SideBarArrow, { target: document.body })
    const tab = () => document.querySelector('button')!
    const dot = () => tab().querySelector('[data-sync-dot]')?.getAttribute('data-sync-dot')
    await tick()
    expect(tab().getAttribute('aria-label')).toBe(language.menu)
    expect(dot()).toBeUndefined()

    syncStatus.set(server({})); await tick()
    expect(dot()).toBeUndefined()
    expect(tab().classList.contains('opacity-50')).toBe(true)

    syncStatus.set(server({ running: true })); await tick()
    expect(dot()).toBe('working')
    expect(tab().classList.contains('opacity-50')).toBe(false)
    expect(tab().getAttribute('aria-label')).toBe(`${language.menu}, ${language.risuNest.tabs.sync}: ${language.risuNest.serverSync.running}`)

    syncStatus.set(server({ paused: true })); await tick()
    expect(dot()).toBe('paused')

    syncStatus.set(server({})); saveFailure.set(new Error('disk full')); await tick()
    expect(dot()).toBe('attention')
    expect(tab().getAttribute('aria-label')).toBe(`${language.menu}, ${language.risuNest.tabs.sync}: ${language.risuNest.syncStatus.saveFailed}`)

    DBState.db.risunestSyncStatusUi = false; await tick()
    expect(dot()).toBeUndefined()
    expect(tab().getAttribute('aria-label')).toBe(language.menu)
    expect(tab().classList.contains('opacity-50')).toBe(true)
})
