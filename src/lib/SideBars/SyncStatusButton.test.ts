vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule())
import { afterEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get } from 'svelte/store'
import { language } from 'src/lang'
import { syncStatusPanel } from 'src/ts/gui/syncStatusPanel'
import { DBState } from 'src/ts/stores.svelte'
import { syncStatus, type SyncStatusSource } from 'src/ts/storage/sync/syncStatus'
import type { ServerSyncView } from 'src/ts/storage/sync/syncStatusView'
import type { ExternalConnectionSummary } from 'src/ts/storage/sync/external/types'
import { externalStorageStrings } from 'src/lib/Setting/ExternalStorage/strings'
import SyncStatusButton from './SyncStatusButton.svelte'

let component: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    document.body.replaceChildren()
    syncStatus.set(null)
    syncStatusPanel.set(null)
    DBState.db.risunestSyncStatusUi = true
})
const server = (view: Partial<ServerSyncView>): SyncStatusSource => ({
    target: 'server',
    view: { status: { configured: true, bound: true }, running: false, paused: false, error: '', ...view } as ServerSyncView,
    pendingChanges: async () => 0,
    syncNow: async () => {},
})

it('offers the status only with a sync binding while the sync UI is turned on', async () => {
    component = mount(SyncStatusButton, { target: document.body, props: { variant: 'slot' } })
    const button = () => document.querySelector<HTMLButtonElement>('button[data-sync-status]')
    await tick()
    expect(document.body.querySelector('button')).toBeNull()

    syncStatus.set(server({})); await tick()
    expect(button()?.getAttribute('aria-label')).toBe(`${language.risuNest.tabs.sync}: ${language.risuNest.serverSync.ready}`)
    button()!.click(); await tick()
    expect(get(syncStatusPanel)).toEqual({ anchor: button(), placement: 'side' })

    DBState.db.risunestSyncStatusUi = false; await tick()
    expect(document.body.querySelector('button')).toBeNull()
})

it('labels the sidebar column as sync and names a failing attempt without a stop', async () => {
    component = mount(SyncStatusButton, { target: document.body, props: { variant: 'menu' } })
    syncStatus.set(server({ error: 'server-unreachable' })); await tick()
    const button = document.querySelector<HTMLButtonElement>('button[data-sync-status]')!
    expect(button.getAttribute('data-sync-status')).toBe('failed')
    expect(button.getAttribute('aria-label')).toBe(`${language.risuNest.tabs.sync}: ${language.risuNest.serverSync.failed}`)
    expect(button.textContent?.trim()).toBe(language.risuNest.syncStatus.menuLabel)
})

it('keeps external storage working between receive turns while changes remain', async () => {
    component = mount(SyncStatusButton, { target: document.body, props: { variant: 'slot' } })
    const connection = { id: 'webdav-1', purpose: 'sync', status: 'ready' } as Partial<ExternalConnectionSummary> as ExternalConnectionSummary
    const source: SyncStatusSource = { target: 'external', connection, syncStopped: false, catchingUp: true, refresh: () => {}, syncNow: async () => {} }
    syncStatus.set(source); await tick()
    const button = () => document.querySelector<HTMLButtonElement>('button[data-sync-status]')!
    expect(button().getAttribute('data-sync-status')).toBe('working')
    expect(button().getAttribute('aria-label')).toBe(`${language.risuNest.tabs.sync}: ${externalStorageStrings('en').transfer.syncing}`)
    syncStatus.set({ ...source, catchingUp: false }); await tick()
    expect(button().getAttribute('data-sync-status')).toBe('connected')
})
it('names an external format level stop as the shared settings badge does', async () => {
    component = mount(SyncStatusButton, { target: document.body, props: { variant: 'slot' } })
    const connection = { id: 'webdav-1', purpose: 'sync', status: 'ready' } as Partial<ExternalConnectionSummary> as ExternalConnectionSummary
    syncStatus.set({ target: 'external', connection, failure: { kind: 'appUpdateRequired' }, syncStopped: false, refresh: () => {}, syncNow: async () => {} }); await tick()
    const button = document.querySelector<HTMLButtonElement>('button[data-sync-status]')!
    expect(button.getAttribute('data-sync-status')).toBe('attention')
    expect(button.getAttribute('aria-label')).toBe(`${language.risuNest.tabs.sync}: ${language.lwwSync.appUpdateRequired}`)
})
