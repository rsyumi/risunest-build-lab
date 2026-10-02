// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
const f = vi.hoisted(() => ({ connect: vi.fn(), configure: vi.fn(), bind: vi.fn(), native: true }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish, changeLanguage: vi.fn() }))
vi.mock('src/ts/platform', () => ({ get isTauri() { return f.native } }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(), alertError: vi.fn(), alertNormal: vi.fn(), openRisuAccountLogin: vi.fn() }))
vi.mock('src/ts/storage/sync/serverSyncProduction', () => ({
    connectServerSync: f.connect, configureServerSyncConnection: f.configure, disconnectServerSync: vi.fn(), retryServerSync: vi.fn(),
    getServerSyncCacheUsage: vi.fn(), cleanupServerSyncCache: vi.fn(),
    getServerSyncController: () => ({ snapshot: () => ({ status: { configured: false }, paused: false }), subscribe: () => () => {}, ensureStatus: vi.fn() }),
}))
vi.mock('src/ts/storage/sync/serverAssetResidency', () => ({ getAssetResidencyStatus: vi.fn(), setAssetResidencyPolicy: vi.fn(), evictLocalAssets: vi.fn(), cancelAssetResidencyOperation: vi.fn() }))
vi.mock('src/ts/storage/sync/serverSyncRegistration', () => ({ parseServerRegistration: () => ({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }) }))
vi.mock('src/ts/storage/sync/serverSyncRegistrationInbox', () => ({ serverRegistrationInbox: { changed: { subscribe: () => () => {} }, releaseConsumed: vi.fn() } }))
vi.mock('src/ts/storage/sync/serverSyncQr', () => ({ canScanServerRegistration: false, createServerQrScanner: () => ({ cancel: vi.fn() }) }))
vi.mock('src/ts/storage/sync/bindingRegistry', () => ({ bindSyncTarget: f.bind }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountOperations', () => ({ restoreNativeOfficialAccountBackup: vi.fn() }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountFlow', () => ({ NativeAccountLoginError: class extends Error {}, getNativeOfficialAccountFlow: vi.fn() }))
vi.mock('src/ts/storage/fileOperationErrorPresentation', () => ({ presentFileOperationError: vi.fn() }))
vi.mock('src/ts/characterCards', () => ({ hubURL: 'https://synthetic.invalid' }))
vi.mock('src/ts/globalApi.svelte', () => ({ getVersionString: () => 'synthetic' }))
vi.mock('src/ts/gui/colorscheme', () => ({ updateTextThemeAndCSS: vi.fn() }))
vi.mock('src/ts/gui/nativeFileJobDialogModel', () => ({ buildNativeFileJobDialogModel: () => ({ open: false }) }))
vi.mock('src/ts/process/templates/templates', () => ({ prebuiltPresets: {} }))
vi.mock('src/ts/storage/database.svelte', () => ({ setPreset: vi.fn() }))
vi.mock('src/ts/storage/nativeFileJobManager', async () => { const { writable } = await import('svelte/store'); return { cancelActiveNativeFileOperation: vi.fn(), dismissNativeFileOperationOutcome: vi.fn(), nativeFileJobHost: writable('dialog'), nativeFileOperation: writable(null), nativeFileOperationOutcome: writable(null) } })
vi.mock('src/ts/storage/officialAccountMessage', () => ({ isExpectedHubMessage: vi.fn(), resolveExpectedOfficialAccountMessageUrl: vi.fn() }))
vi.mock('src/ts/storage/portableBackupFileRouteProduction.svelte', () => ({ restoreBackupFromSystemPicker: vi.fn() }))
vi.mock('src/ts/storage/risuSaveFileRouteProduction.svelte', () => ({ importRisuSaveFromSystemPicker: vi.fn() }))
vi.mock('src/ts/storage/sync/external/bridge', () => ({ getExternalStorageBridge: () => ({}) }))
vi.mock('src/ts/storage/sync/external/production', () => ({ refreshExternalStorageProductionState: vi.fn(), requestExternalStorageRestore: vi.fn() }))
vi.mock('src/lib/Setting/ExternalStorage/ConnectionForm.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/Others/Onboarding/onboardingWeave', () => ({ observeOnboardingWeave: () => () => {} }))
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: { language: 'en' } } }))
import ServerSyncSettings from './ServerSyncSettings.svelte'
import Onboarding from 'src/lib/Others/Onboarding/Onboarding.svelte' 
let component: ReturnType<typeof mount> | undefined
let host: HTMLDivElement
beforeEach(() => { vi.clearAllMocks(); f.native = true; host = document.createElement('div'); document.body.append(host) })
afterEach(async () => { if (component) await unmount(component); component = undefined; host.remove() })
const settle = async () => { for (let i = 0; i < 12; i++) await tick() }
function click(text: string) { const button = [...host.querySelectorAll('button')].find(button => button.textContent?.trim() === text); expect(button).toBeDefined(); button!.click() }
async function registration() { await tick(); const input = host.querySelector('textarea')!; input.value = 'synthetic-registration'; input.dispatchEvent(new Event('input', { bubbles: true })); await tick(); click(languageEnglish.risuNest.serverSync.readRegistration); await tick() }
it.each([false, true])('uses the supplied onboarding action once with newDevice=%s', async newDevice => {
    const connectTarget = vi.fn(async () => {})
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget } })
    await registration()
    click(newDevice ? languageEnglish.lwwSync.newDeviceAction : languageEnglish.risuNest.serverSync.connect)
    await settle()
    expect(connectTarget).toHaveBeenCalledExactlyOnceWith({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }, newDevice)
    expect(f.connect).not.toHaveBeenCalled()
})
it('keeps settings on the existing production action by default', async () => {
    component = mount(ServerSyncSettings, { target: host })
    await registration(); click(languageEnglish.risuNest.serverSync.connect); await settle()
    expect(f.connect).toHaveBeenCalledExactlyOnceWith({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }, false)
})
it('dismisses a registration without connecting', async () => {
    const connectTarget = vi.fn()
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget } })
    await registration(); click(languageEnglish.risuNest.serverSync.discardRegistration); await settle()
    expect(connectTarget).not.toHaveBeenCalled(); expect(f.connect).not.toHaveBeenCalled()
})
it('hides the native server connection UI on web', async () => {
    f.native = false; component = mount(ServerSyncSettings, { target: host }); await tick()
    expect(host.querySelector('textarea')).toBeNull(); expect(host.querySelector('button')).toBeNull()
})

async function openOnboardingServer() {
    component = mount(Onboarding, { target: host }); await tick()
    host.querySelectorAll('button').forEach(button => { if (button.querySelector('b')?.textContent === languageEnglish.risuNest.onboarding.home.syncTitle) button.click() })
    await tick()
    const entry = [...host.querySelectorAll('button')].find(button => button.querySelector('b')?.textContent === languageEnglish.risuNest.onboarding.sync.hubTitle)
    expect(entry).toBeDefined(); entry!.click(); await tick()
    expect(host.querySelector('h1')?.textContent).toBe(languageEnglish.risuNest.onboarding.hub.title)
}
it.each(['bound', 'cancelled'])('mounted onboarding advances only after %s shared binding outcome', async kind => {
    f.configure.mockResolvedValue(undefined); f.bind.mockResolvedValue({ kind })
    await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.connect); await settle()
    expect(f.configure).toHaveBeenCalledTimes(1); expect(f.bind).toHaveBeenCalledExactlyOnceWith({ kind: 'server', connectionId: 'server' }, undefined)
    expect(f.connect).not.toHaveBeenCalled()
    expect(host.querySelector('h1')?.textContent).toBe(kind === 'bound' ? languageEnglish.risuNest.onboarding.done.title : languageEnglish.risuNest.onboarding.hub.title)
})
it('mounted web onboarding offers no native server entry', async () => {
    f.native = false; component = mount(Onboarding, { target: host }); await tick()
    expect(host.textContent).not.toContain(languageEnglish.risuNest.onboarding.sync.hubTitle)
    expect(host.querySelector('#server-registration')).toBeNull()
})
it('mounted onboarding discards the registration without advancing', async () => {
    await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.discardRegistration); await settle()
    expect(f.bind).not.toHaveBeenCalled(); expect(f.configure).not.toHaveBeenCalled()
    expect(host.querySelector('h1')?.textContent).toBe(languageEnglish.risuNest.onboarding.hub.title)
})

it('mounted onboarding keeps binding failures on the connection screen', async () => {
    f.configure.mockResolvedValue(undefined); f.bind.mockRejectedValue(new Error('refresh-failed'))
    await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.connect); await settle()
    expect(host.querySelector('h1')?.textContent).toBe(languageEnglish.risuNest.onboarding.hub.title)
    expect(host.querySelector('[role="alert"]')).not.toBeNull()
})
it('a late bound response does not advance a dismissed server screen', async () => {
    let finish!: (value: { kind: string }) => void
    f.configure.mockResolvedValue(undefined); f.bind.mockImplementation(() => new Promise(resolve => { finish = resolve }))
    await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.connect); await settle()
    click(languageEnglish.risuNest.onboarding.back); await tick()
    finish({ kind: 'bound' }); await settle()
    expect(host.querySelector('h1')?.textContent).toBe(languageEnglish.risuNest.onboarding.sync.title)
})
