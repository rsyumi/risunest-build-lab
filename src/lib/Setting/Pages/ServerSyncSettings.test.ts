// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
const f = vi.hoisted(() => ({ connect: vi.fn(), configure: vi.fn(), bind: vi.fn(), disconnect: vi.fn(), hold: vi.fn(), release: vi.fn(), status: vi.fn(), policy: vi.fn(), cancel: vi.fn(), checkbox: vi.fn(), native: true, view: { status: { configured: false }, paused: false } as Record<string, unknown> }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish, changeLanguage: vi.fn() }))
vi.mock('src/ts/platform', () => ({ get isTauri() { return f.native } }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(), alertCheckboxConfirm: f.checkbox, alertError: vi.fn(), alertNormal: vi.fn(), openRisuAccountLogin: vi.fn() }))
vi.mock('src/ts/storage/sync/serverSyncProduction', () => ({
    connectServerSync: f.connect, configureServerSyncConnection: f.configure, disconnectServerSync: f.disconnect, retryServerSync: vi.fn(), holdServerSync: f.hold,
    getServerSyncCacheUsage: vi.fn(), cleanupServerSyncCache: vi.fn(),
    getServerSyncController: () => ({ snapshot: () => f.view, subscribe: () => () => {}, ensureStatus: vi.fn() }),
}))
vi.mock('src/ts/storage/sync/serverAssetResidency', () => ({ getAssetResidencyStatus: f.status, setAssetResidencyPolicy: f.policy, evictLocalAssets: vi.fn(), cancelAssetResidencyOperation: f.cancel }))
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
beforeEach(() => {
    vi.clearAllMocks(); f.native = true; f.view = { status: { configured: false }, paused: false }; host = document.createElement('div'); document.body.append(host)
    for (const mock of [f.disconnect, f.hold, f.release, f.status, f.policy, f.cancel, f.checkbox]) mock.mockReset()
    f.hold.mockResolvedValue(f.release)
})
afterEach(async () => { if (component) await unmount(component); component = undefined; host.remove() })
const settle = async () => { for (let i = 0; i < 12; i++) await tick() }
function click(text: string) { const button = [...host.querySelectorAll('button')].find(button => button.textContent?.trim() === text); expect(button).toBeDefined(); button!.click() }
async function registration() { await tick(); const input = host.querySelector('textarea')!; input.value = 'synthetic-registration'; input.dispatchEvent(new Event('input', { bubbles: true })); await tick(); click(languageEnglish.risuNest.serverSync.readRegistration); await tick() }
const findButton = (text: string) => [...host.querySelectorAll('button')].find(button => button.textContent?.trim() === text)
it.each([
    { error: '', newDevice: false },
    { error: 'writer-collision', newDevice: false },
    { error: 'writer-collision', newDevice: true },
    { error: 'equal-stamp-integrity', newDevice: true },
])('uses the supplied onboarding action once with newDevice=$newDevice in state "$error"', async ({ error, newDevice }) => {
    f.view = { status: { configured: true, bound: !!error }, paused: !!error, error }
    const connectTarget = vi.fn(async () => {})
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget } })
    await registration()
    click(newDevice ? languageEnglish.lwwSync.newDeviceAction : languageEnglish.risuNest.serverSync.connect)
    await settle()
    expect(connectTarget).toHaveBeenCalledExactlyOnceWith({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }, newDevice)
    expect(f.connect).not.toHaveBeenCalled()
})
it.each(['', 'server-unreachable', 'clock-skew', 'unauthorized'])('offers no new-device connection outside the duplicate-device recovery state ("%s")', async error => {
    f.view = { status: { configured: true, bound: !!error }, paused: !!error, error }
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await registration()
    expect(findButton(languageEnglish.risuNest.serverSync.connect)).toBeDefined()
    expect(findButton(languageEnglish.lwwSync.newDeviceAction)).toBeUndefined()
})
it('explains an item too large to send instead of the generic sync error', async () => {
    f.view = { status: { configured: true, bound: true }, paused: true, error: 'unit-too-large' }
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    const alert = host.querySelector('[role="alert"]')
    expect(alert?.textContent).toBe(languageEnglish.lwwSync.unitTooLarge)
    expect(alert?.textContent).not.toBe(languageEnglish.risuNest.serverSync.errorHelp)
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

const sync = languageEnglish.risuNest.serverSync
const residencyStatus = (overrides: Record<string, unknown> = {}) => ({ policy: 'remote', localBytes: 0, remoteBytes: 4096, remoteObjects: 2, unavailableObjects: 0, evictedBytes: 0, ...overrides })
async function mountBound(status: Record<string, unknown> = residencyStatus()) {
    f.view = { status: { configured: true, bound: true }, paused: false, error: '' }
    f.status.mockResolvedValue(status)
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
}
const alertText = () => host.querySelector('[role="alert"]')?.textContent
describe('disconnecting with files kept only on the server', () => {
    it('disconnects without asking when no file is kept only on the server', async () => {
        await mountBound(residencyStatus({ remoteObjects: 0, remoteBytes: 0 }))
        click(sync.disconnect); await settle()
        expect(f.checkbox).not.toHaveBeenCalled(); expect(f.policy).not.toHaveBeenCalled()
        expect(f.disconnect).toHaveBeenCalledOnce()
    })
    it('reads the current status when disconnecting and asks once with an unchecked download option', async () => {
        await mountBound(residencyStatus({ remoteObjects: 0, remoteBytes: 0 }))
        f.status.mockResolvedValue(residencyStatus())
        f.checkbox.mockResolvedValue({ confirmed: false, checked: false })
        click(sync.disconnect); await settle()
        expect(f.checkbox).toHaveBeenCalledExactlyOnceWith({
            title: sync.disconnectTitle, description: sync.disconnectRemoteOnly, checkboxLabel: sync.downloadThenDisconnect,
            actionLabel: sync.disconnect, cancelLabel: languageEnglish.cancel, requireChecked: false,
        })
        expect(f.disconnect).not.toHaveBeenCalled(); expect(f.policy).not.toHaveBeenCalled(); expect(f.hold).not.toHaveBeenCalled()
    })
    it('disconnects without downloading when the download option stays unchecked', async () => {
        await mountBound()
        f.checkbox.mockResolvedValue({ confirmed: true, checked: false })
        click(sync.disconnect); await settle()
        expect(f.disconnect).toHaveBeenCalledOnce(); expect(f.policy).not.toHaveBeenCalled(); expect(f.hold).not.toHaveBeenCalled()
    })
    it('stops sync, downloads every file, then disconnects', async () => {
        await mountBound()
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockResolvedValue(residencyStatus({ policy: 'full', remoteObjects: 0, remoteBytes: 0 }))
        click(sync.disconnect); await settle()
        expect(f.policy).toHaveBeenCalledExactlyOnceWith('full')
        expect(f.disconnect).toHaveBeenCalledOnce(); expect(f.release).toHaveBeenCalledOnce()
        const order = [f.hold, f.policy, f.disconnect, f.release].map(mock => mock.mock.invocationCallOrder[0])
        expect(order).toEqual([...order].sort((a, b) => a - b))
        expect(alertText()).toBeUndefined()
    })
    it('shows the busy text and the cancel button while the download runs', async () => {
        await mountBound()
        let finish!: () => void
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockImplementation(() => new Promise(resolve => { finish = () => resolve(residencyStatus({ policy: 'full', remoteObjects: 0 })) }))
        click(sync.disconnect); await settle()
        expect(host.querySelector('[role="status"]')?.textContent).toBe(sync.residency.working)
        click(sync.residency.cancel); await settle()
        expect(f.cancel).toHaveBeenCalledOnce()
        finish(); await settle()
        expect(host.querySelector('[role="status"]')).toBeNull()
    })
    it('keeps the connection and explains it when the download fails', async () => {
        await mountBound()
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'server-unreachable', retryable: true })
        click(sync.disconnect); await settle()
        expect(f.disconnect).not.toHaveBeenCalled(); expect(f.release).toHaveBeenCalledOnce()
        expect(alertText()).toBe(sync.downloadFailedKeptConnection)
    })
    it('disconnects when the failed download leaves no file only on the server', async () => {
        await mountBound()
        f.status.mockResolvedValueOnce(residencyStatus()).mockResolvedValueOnce(residencyStatus({ remoteObjects: 0, remoteBytes: 0, unavailableObjects: 1 }))
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'required-asset-unavailable', retryable: false })
        click(sync.disconnect); await settle()
        expect(f.disconnect).toHaveBeenCalledOnce(); expect(f.release).toHaveBeenCalledOnce()
        const order = [f.hold, f.policy, f.disconnect, f.release].map(mock => mock.mock.invocationCallOrder[0])
        expect(order).toEqual([...order].sort((a, b) => a - b))
        expect(alertText()).toBeUndefined()
    })
    it.each([
        { name: 'still lists files only on the server', reread: () => f.status.mockResolvedValueOnce(residencyStatus()).mockResolvedValueOnce(residencyStatus({ remoteObjects: 1, unavailableObjects: 1 })) },
        { name: 'cannot be read', reread: () => f.status.mockResolvedValueOnce(residencyStatus()).mockRejectedValueOnce(new Error('status-unavailable')) },
    ])('keeps the connection when the status after a failed download $name', async ({ reread }) => {
        await mountBound()
        reread()
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'required-asset-unavailable', retryable: false })
        click(sync.disconnect); await settle()
        expect(f.disconnect).not.toHaveBeenCalled(); expect(f.release).toHaveBeenCalledOnce()
        expect(alertText()).toBe(sync.downloadFailedKeptConnection)
    })
    it('keeps the connection without a message when the download is cancelled', async () => {
        await mountBound()
        f.status.mockResolvedValueOnce(residencyStatus()).mockResolvedValue(residencyStatus({ remoteObjects: 0, remoteBytes: 0 }))
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'cancelled', retryable: true })
        click(sync.disconnect); await settle()
        expect(f.disconnect).not.toHaveBeenCalled(); expect(f.release).toHaveBeenCalledOnce()
        expect(alertText()).toBeUndefined()
    })
    it('disconnects without asking when the status cannot be read', async () => {
        await mountBound()
        f.status.mockRejectedValueOnce(new Error('status-unavailable'))
        click(sync.disconnect); await settle()
        expect(f.checkbox).not.toHaveBeenCalled(); expect(f.policy).not.toHaveBeenCalled()
        expect(f.disconnect).toHaveBeenCalledOnce()
    })
})
describe('downloading files kept only on the server', () => {
    it.each([
        { policy: 'full', remoteObjects: 2, shown: true },
        { policy: 'full', remoteObjects: 0, shown: false },
        { policy: 'remote', remoteObjects: 2, shown: false },
    ])('offers the download for policy $policy with $remoteObjects server-only files: $shown', async ({ policy, remoteObjects, shown }) => {
        await mountBound(residencyStatus({ policy, remoteObjects }))
        expect(!!findButton(sync.residency.download)).toBe(shown)
    })
    it('downloads with sync stopped and stays connected', async () => {
        await mountBound(residencyStatus({ policy: 'full' }))
        f.policy.mockResolvedValue(residencyStatus({ policy: 'full', remoteObjects: 0, remoteBytes: 0 }))
        f.status.mockResolvedValue(residencyStatus({ policy: 'full', remoteObjects: 0, remoteBytes: 0 }))
        click(sync.residency.download); await settle()
        expect(f.policy).toHaveBeenCalledExactlyOnceWith('full'); expect(f.disconnect).not.toHaveBeenCalled()
        const order = [f.hold, f.policy, f.release].map(mock => mock.mock.invocationCallOrder[0])
        expect(order).toEqual([...order].sort((a, b) => a - b))
        expect(findButton(sync.residency.download)).toBeUndefined()
    })
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
