import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import { get } from 'svelte/store'
import type { PlatformShape } from 'src/ts/platform.testSupport'

const env = vi.hoisted(() => ({ shape: { runtime: 'tauri', os: 'windows' } as PlatformShape }))
// Each child renders as a marker carrying its name, so the page's own choices are what is tested.
const child = vi.hoisted(() => (name: string) => async () => {
    const { default: Stub } = await import('./RisuNestSettingsChildStub.test.svelte')
    return { default: (anchor: never, props: Record<string, unknown>) => Stub(anchor, { ...props, name }) }
})

vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule(() => env.shape))
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: { language: 'en' } } }))
vi.mock('../RisuNest/RisuNestSettingRows.svelte', async () => {
    const { default: Stub } = await import('./RisuNestSettingsChildStub.test.svelte')
    const data = await import('src/ts/setting/risuNestSettingsData')
    const groups = new Map<unknown, string>([
        [data.risuNestUiSettingsItems, 'ui'],
        [data.risuNestStreamingSettingsItems, 'streaming'],
        [data.risuNestInlaySettingsItems, 'attachments'],
    ])
    return { default: (anchor: never, props: { items: unknown }) => Stub(anchor, { name: `rows:${groups.get(props.items)}` }) }
})
vi.mock('./RisuNestStartupExclusions.svelte', child('startup-exclusions'))
vi.mock('./RisuNestPerformanceSettings.svelte', child('performance'))
vi.mock('./RisuNestUpdateSettings.svelte', child('update'))
vi.mock('./RisuNestAppImage.svelte', child('app-image'))
vi.mock('./RisuNestIOSPlatform.svelte', child('ios-platform'))
vi.mock('./RisuNestAndroidPlatform.svelte', child('android-platform'))
vi.mock('./RisuNestLogViewer.svelte', child('log-viewer'))
vi.mock('./RisuNestStorageDashboard.svelte', child('storage-dashboard'))
vi.mock('./RisuNestInlayInventory.svelte', child('inlay-inventory'))
vi.mock('./RisuNestImageGeometry.svelte', child('image-geometry'))
vi.mock('./RisuNestDataHealth.svelte', child('data-health'))
vi.mock('./RisuNestBackupRestore.svelte', child('backup-restore'))
vi.mock('../RisuNest/LocalDataReset.svelte', child('local-data-reset'))
vi.mock('./ServerSyncSettings.svelte', child('server-sync'))
vi.mock('../ExternalStorage/ExternalStorageSettings.svelte', child('external-storage'))
vi.mock('./RisuNestLocalData.svelte', child('local-data'))
vi.mock('./RisuNestNetworkLog.svelte', child('network-log'))
vi.mock('./RisuNestPluginData.svelte', child('plugin-data'))

import { language } from 'src/lang'
import { risuNestSettingsTabRequest, type RisuNestSettingsTab } from 'src/ts/setting/risuNestSettingsTabs'
import RisuNestSettings from './RisuNestSettings.svelte'

const desktop: PlatformShape = { runtime: 'tauri', os: 'windows' }
const android: PlatformShape = { runtime: 'tauri', os: 'android' }
const ios: PlatformShape = { runtime: 'tauri', os: 'ios' }
const web: PlatformShape = { runtime: 'web', os: 'windows' }

let component: ReturnType<typeof mount> | undefined
let target: HTMLElement

function render(shape: PlatformShape, request: RisuNestSettingsTab | null = null) {
    env.shape = shape
    risuNestSettingsTabRequest.set(request)
    target = document.createElement('div')
    document.body.append(target)
    component = mount(RisuNestSettings, { target })
    flushSync()
}

const tabs = () => [...target.querySelectorAll<HTMLButtonElement>('[role="tab"]')]
const selectedTab = () => tabs().find((tab) => tab.getAttribute('aria-selected') === 'true')?.dataset.risunestTab
const panelChildren = () => {
    const panel = target.querySelector<HTMLElement>('[role="tabpanel"]')!
    expect(panel.getAttribute('aria-labelledby')).toBe(tabs().find((tab) => tab.getAttribute('aria-selected') === 'true')?.id)
    return [...panel.querySelectorAll<HTMLElement>('[data-child]')].map((element) => element.dataset.child)
}
function open(tab: RisuNestSettingsTab) {
    tabs().find((button) => button.dataset.risunestTab === tab)!.click()
    flushSync()
}

afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    target?.remove()
    risuNestSettingsTabRequest.set(null)
})

describe('RisuNest settings tabs', () => {
    it('offers the network log tab only in the native app', () => {
        render(desktop)
        expect(tabs().map((tab) => tab.dataset.risunestTab)).toEqual(['settings', 'storage', 'sync', 'plugin-data', 'network-logs'])
        expect(tabs().map((tab) => tab.textContent)).toEqual([
            language.risuNest.tabs.settings, language.risuNest.tabs.storage, language.risuNest.tabs.sync,
            language.risuNest.tabs.pluginData, language.risuNest.tabs.networkLogs,
        ])
        void unmount(component!)
        target.remove()
        render(web)
        expect(tabs().map((tab) => tab.dataset.risunestTab)).toEqual(['settings', 'storage', 'sync', 'plugin-data'])
    })

    it.each([
        ['Windows', desktop, ['update', 'app-image', 'log-viewer']],
        ['Android', android, ['update', 'android-platform', 'log-viewer']],
        ['iOS', ios, ['update', 'ios-platform', 'log-viewer']],
        ['the web', web, []],
    ] as const)('orders the settings tab with the platform sections for %s', (_name, shape, native) => {
        render(shape)
        expect(selectedTab()).toBe('settings')
        expect(panelChildren()).toEqual([
            'startup-exclusions', 'performance', 'rows:ui', 'rows:streaming', 'rows:attachments', ...native,
        ])
    })

    it('orders the storage and sync tabs, keeping native sections out of the web build', () => {
        render(desktop)
        open('storage')
        expect(panelChildren()).toEqual([
            'storage-dashboard', 'inlay-inventory', 'image-geometry', 'data-health', 'backup-restore', 'local-data-reset',
        ])
        open('sync')
        expect(panelChildren()).toEqual(['server-sync', 'external-storage', 'local-data'])
        open('plugin-data')
        expect(panelChildren()).toEqual(['plugin-data'])
        open('network-logs')
        expect(panelChildren()).toEqual(['network-log'])

        void unmount(component!)
        target.remove()
        render(web)
        open('storage')
        expect(panelChildren()).toEqual(['inlay-inventory', 'backup-restore', 'local-data-reset'])
        open('sync')
        expect(panelChildren()).toEqual(['external-storage'])
    })

    it('scrolls to the storage section when the data check opens unused images', () => {
        render(desktop)
        open('storage')
        const storage = document.createElement('section')
        storage.id = 'risunest-storage'
        storage.scrollIntoView = vi.fn()
        document.body.append(storage)
        target.querySelector<HTMLButtonElement>('[data-open-unused-images]')!.click()
        expect(storage.scrollIntoView).toHaveBeenCalledOnce()
        storage.remove()
    })

    it('opens the tab a navigation request names and clears the request', () => {
        render(desktop, 'sync')
        expect(selectedTab()).toBe('sync')
        expect(get(risuNestSettingsTabRequest)).toBeNull()
        risuNestSettingsTabRequest.set('storage')
        flushSync()
        expect(selectedTab()).toBe('storage')
        expect(panelChildren()).toContain('backup-restore')
    })

    it('falls back to the first tab when the requested tab is not offered', () => {
        render(web)
        open('sync')
        risuNestSettingsTabRequest.set('network-logs')
        flushSync()
        expect(selectedTab()).toBe('settings')
        expect(get(risuNestSettingsTabRequest)).toBeNull()
    })

    it('moves between tabs with the arrow, Home and End keys', () => {
        render(desktop)
        const press = (key: string) => {
            document.activeElement!.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }))
            flushSync()
            return (document.activeElement as HTMLElement).dataset.risunestTab
        }
        tabs()[0].focus()
        expect(press('ArrowLeft')).toBe('network-logs')
        expect(selectedTab()).toBe('network-logs')
        expect(press('ArrowRight')).toBe('settings')
        expect(press('End')).toBe('network-logs')
        expect(press('Home')).toBe('settings')
        expect(press('ArrowRight')).toBe('storage')
        expect(tabs().map((tab) => tab.tabIndex)).toEqual([-1, 0, -1, -1, -1])
    })
})
