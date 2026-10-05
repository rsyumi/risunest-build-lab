// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
const f = vi.hoisted(() => ({ connect: vi.fn(), complete: vi.fn(), configure: vi.fn(), bind: vi.fn(), disconnect: vi.fn(), hold: vi.fn(), release: vi.fn(), status: vi.fn(), policy: vi.fn(), cancel: vi.fn(), checkbox: vi.fn(), native: true, scan: false, os: 'windows', listeners: new Set<(value: Record<string, unknown>) => void>(), bindingState: vi.fn(), preset: vi.fn(), state: { db: {} as Record<string, unknown> }, view: { status: { configured: false }, paused: false } as Record<string, unknown> }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish, changeLanguage: vi.fn() }))
vi.mock('src/ts/platform', () => ({ get isTauri() { return f.native } }))
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => f.os }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(), alertCheckboxConfirm: f.checkbox, alertError: vi.fn(), alertNormal: vi.fn(), openRisuAccountLogin: vi.fn() }))
vi.mock('src/ts/storage/sync/serverSyncProduction', () => ({
    connectServerSync: f.connect, completeServerSyncBinding: f.complete, configureServerSyncConnection: f.configure, disconnectServerSync: f.disconnect, retryServerSync: vi.fn(), holdServerSync: f.hold,
    getServerSyncCacheUsage: vi.fn(), cleanupServerSyncCache: vi.fn(),
    getServerSyncController: () => ({ snapshot: () => f.view, subscribe: (listener: (value: Record<string, unknown>) => void) => { f.listeners.add(listener); listener(f.view); return () => { f.listeners.delete(listener) } }, ensureStatus: vi.fn() }),
}))
vi.mock('src/ts/storage/sync/serverAssetResidency', () => ({ getAssetResidencyStatus: f.status, setAssetResidencyPolicy: f.policy, evictLocalAssets: vi.fn(), cancelAssetResidencyOperation: f.cancel }))
vi.mock('src/ts/storage/sync/serverSyncRegistration', () => ({ parseServerRegistration: () => ({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }) }))
vi.mock('src/ts/storage/sync/serverSyncRegistrationInbox', () => ({ serverRegistrationInbox: { changed: { subscribe: () => () => {} }, releaseConsumed: vi.fn() } }))
vi.mock('src/ts/storage/sync/serverSyncQr', () => ({ get canScanServerRegistration() { return f.scan }, createServerQrScanner: () => ({ cancel: vi.fn() }) }))
vi.mock('src/ts/storage/sync/bindingRegistry', () => ({ bindSyncTarget: f.bind }))
vi.mock('src/ts/storage/sync/bindingNative', () => ({ createNativeSyncBindingBridge: () => ({ state: f.bindingState }) }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountOperations', () => ({ restoreNativeOfficialAccountBackup: vi.fn() }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountFlow', () => ({ NativeAccountLoginError: class extends Error {}, getNativeOfficialAccountFlow: vi.fn() }))
vi.mock('src/ts/storage/fileOperationErrorPresentation', () => ({ presentFileOperationError: vi.fn() }))
vi.mock('src/ts/characterCards', () => ({ hubURL: 'https://synthetic.invalid' }))
vi.mock('src/ts/globalApi.svelte', () => ({ getVersionString: () => 'synthetic' }))
vi.mock('src/ts/gui/colorscheme', () => ({ updateTextThemeAndCSS: vi.fn() }))
vi.mock('src/ts/gui/nativeFileJobDialogModel', () => ({ buildNativeFileJobDialogModel: () => ({ open: false }) }))
vi.mock('src/ts/process/templates/templates', () => ({ prebuiltPresets: {} }))
vi.mock('src/ts/storage/database.svelte', () => ({ setPreset: f.preset }))
vi.mock('src/ts/storage/nativeFileJobManager', async () => { const { writable } = await import('svelte/store'); return { cancelActiveNativeFileOperation: vi.fn(), dismissNativeFileOperationOutcome: vi.fn(), nativeFileJobHost: writable('dialog'), nativeFileOperation: writable(null), nativeFileOperationOutcome: writable(null) } })
vi.mock('src/ts/storage/officialAccountMessage', () => ({ isExpectedHubMessage: vi.fn(), resolveExpectedOfficialAccountMessageUrl: vi.fn() }))
vi.mock('src/ts/storage/portableBackupFileRouteProduction.svelte', () => ({ restoreBackupFromSystemPicker: vi.fn() }))
vi.mock('src/ts/storage/risuSaveFileRouteProduction.svelte', () => ({ importRisuSaveFromSystemPicker: vi.fn() }))
vi.mock('src/ts/storage/sync/external/bridge', () => ({ getExternalStorageBridge: () => ({}) }))
vi.mock('src/ts/storage/sync/external/production', () => ({ refreshExternalStorageProductionState: vi.fn(), requestExternalStorageRestore: vi.fn() }))
vi.mock('src/lib/Setting/ExternalStorage/ConnectionForm.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/Others/Onboarding/onboardingWeave', () => ({ observeOnboardingWeave: () => () => {} }))
vi.mock('src/ts/stores.svelte', () => ({ DBState: f.state }))
import ServerSyncSettings from './ServerSyncSettings.svelte'
import Onboarding from 'src/lib/Others/Onboarding/Onboarding.svelte'
let component: ReturnType<typeof mount> | undefined
let host: HTMLDivElement
beforeEach(() => {
    vi.clearAllMocks(); f.native = true; f.scan = false; f.os = 'windows'; f.listeners.clear(); f.view = { status: { configured: false }, paused: false }; host = document.createElement('div'); document.body.append(host)
    for (const mock of [f.disconnect, f.hold, f.release, f.status, f.policy, f.cancel, f.checkbox, f.complete]) mock.mockReset()
    f.hold.mockResolvedValue(f.release)
    f.state.db = { language: 'en', characters: [] }
    f.preset.mockImplementation((db: Record<string, unknown>) => ({ ...db, preset: 'starting' }))
    f.bindingState.mockResolvedValue({ target: { kind: 'none' } })
})
afterEach(async () => { if (component) await unmount(component); component = undefined; host.remove() })
const settle = async () => { for (let i = 0; i < 12; i++) await tick() }
function click(text: string) { const button = [...host.querySelectorAll('button')].find(button => button.textContent?.trim() === text); expect(button).toBeDefined(); button!.click() }
async function registration() { await tick(); const input = host.querySelector('textarea')!; input.value = 'synthetic-registration'; input.dispatchEvent(new Event('input', { bubbles: true })); await tick(); click(languageEnglish.risuNest.serverSync.readRegistration); await tick() }
function publish(view: Record<string, unknown>) { f.view = view; for (const listener of f.listeners) listener(view) }
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
it('shows an unfinished connection with Connect, which finishes it with the saved registration', async () => {
    f.view = { status: { configured: false, bound: true }, paused: true, error: '', bindingIncomplete: true }
    f.complete.mockResolvedValue(undefined)
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    expect(host.querySelector('[role="status"]')?.textContent).toBe(languageEnglish.lwwSync.bindingIncomplete)
    expect(findButton(languageEnglish.risuNest.serverSync.syncNow)).toBeUndefined()
    expect(findButton(languageEnglish.risuNest.serverSync.disconnect)).toBeDefined()
    click(languageEnglish.risuNest.serverSync.connect)
    await settle()
    expect(f.complete).toHaveBeenCalledOnce()
    expect(f.connect).not.toHaveBeenCalled(); expect(f.bind).not.toHaveBeenCalled()
})
it('reports why Connect could not finish an unfinished connection', async () => {
    f.view = { status: { configured: false, bound: true }, paused: true, error: '', bindingIncomplete: true }
    f.complete.mockRejectedValue({ code: 'unauthorized' })
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    click(languageEnglish.risuNest.serverSync.connect)
    await settle()
    expect(host.querySelector('[role="alert"]')?.textContent).toBe(languageEnglish.lwwSync.registrationRevoked)
    expect(host.querySelector('[role="status"]')?.textContent).toBe(languageEnglish.lwwSync.bindingIncomplete)
})
it('shows no unfinished connection outside that state', async () => {
    f.view = { status: { configured: true, bound: true }, paused: false, error: '' }
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    expect(host.textContent).not.toContain(languageEnglish.lwwSync.bindingIncomplete)
    expect(findButton(languageEnglish.risuNest.serverSync.connect)).toBeUndefined()
    expect(findButton(languageEnglish.risuNest.serverSync.syncNow)).toBeDefined()
})
it('explains an item too large to send instead of the generic sync error', async () => {
    f.view = { status: { configured: true, bound: true }, paused: true, error: 'unit-too-large' }
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    const alert = host.querySelector('[role="alert"]')
    expect(alert?.textContent).toBe(languageEnglish.lwwSync.unitTooLarge)
    expect(alert?.textContent).not.toBe(languageEnglish.risuNest.serverSync.errorHelp)
})
it.each([
    { error: 'unauthorized', text: languageEnglish.lwwSync.registrationRevoked },
    { error: 'invalid-device-token', text: languageEnglish.lwwSync.registrationRevoked },
    { error: 'device-credential-unavailable', text: languageEnglish.risuNest.serverSync.credentialUnavailable },
    { error: 'previous-files-download-failed', text: languageEnglish.lwwSync.downloadFailedNotConnected },
    { error: 'previous-storage-unavailable', text: languageEnglish.lwwSync.previousStorageUnavailable },
])('explains "$error" instead of the generic sync error', async ({ error, text }) => {
    f.view = { status: { configured: true, bound: true }, paused: true, error }
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    expect(host.querySelector('[role="alert"]')?.textContent).toBe(text)
})
it.each(['linux', 'windows', 'macos', 'android', 'ios'])('names the Secret Service for an unreadable credential only on Linux (%s)', async os => {
    f.os = os
    f.view = { status: { configured: true, bound: true }, paused: true, error: 'device-credential-unavailable' }
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    const text = host.querySelector('[role="alert"]')?.textContent
    expect(text).toBe(os === 'linux' ? languageEnglish.risuNest.serverSync.credentialUnavailableLinux : languageEnglish.risuNest.serverSync.credentialUnavailable)
    expect(text?.includes('Secret Service')).toBe(os === 'linux')
})
it.each([false, true])('mentions the QR code in the connection help only where it can be scanned (%s)', async scan => {
    f.scan = scan
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    expect(host.textContent).toContain(scan ? languageEnglish.risuNest.serverSync.connectRowHelpScan : languageEnglish.risuNest.serverSync.connectRowHelp)
    expect(host.textContent?.includes('QR')).toBe(scan)
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
const residencyStatus = (overrides: Record<string, unknown> = {}) => ({ policy: 'remote', localBytes: 0, remoteBytes: 4096, remoteObjects: 2, serverBytes: 4096, serverObjects: 2, externalObjects: [], unavailableObjects: 0, evictedBytes: 0, ...overrides })
const externalOnly = { serverObjects: 0, serverBytes: 0, externalObjects: [{ connectionId: 'external', objects: 2 }] }
async function mountBound(status: Record<string, unknown> = residencyStatus()) {
    f.view = { status: { configured: true, bound: true }, paused: false, error: '' }
    f.status.mockResolvedValue(status)
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
}
const alertText = () => host.querySelector('[role="alert"]')?.textContent
describe('disconnecting with files kept only on the server', () => {
    it('disconnects without asking when no file is kept only on the server', async () => {
        await mountBound(residencyStatus({ remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0 }))
        click(sync.disconnect); await settle()
        expect(f.checkbox).not.toHaveBeenCalled(); expect(f.policy).not.toHaveBeenCalled()
        expect(f.disconnect).toHaveBeenCalledOnce()
    })
    it('reads the current status when disconnecting and asks once with an unchecked download option', async () => {
        await mountBound(residencyStatus({ remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0 }))
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
        f.policy.mockResolvedValue(residencyStatus({ policy: 'full', remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0 }))
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
        f.policy.mockImplementation(() => new Promise(resolve => { finish = () => resolve(residencyStatus({ policy: 'full', remoteObjects: 0, serverObjects: 0 })) }))
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
        f.status.mockResolvedValueOnce(residencyStatus()).mockResolvedValueOnce(residencyStatus({ remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0, unavailableObjects: 1 }))
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'required-asset-unavailable', retryable: false })
        click(sync.disconnect); await settle()
        expect(f.disconnect).toHaveBeenCalledOnce(); expect(f.release).toHaveBeenCalledOnce()
        const order = [f.hold, f.policy, f.disconnect, f.release].map(mock => mock.mock.invocationCallOrder[0])
        expect(order).toEqual([...order].sort((a, b) => a - b))
        expect(alertText()).toBeUndefined()
    })
    it.each([
        { name: 'still lists files only on the server', reread: () => f.status.mockResolvedValueOnce(residencyStatus()).mockResolvedValueOnce(residencyStatus({ remoteObjects: 1, serverObjects: 1, unavailableObjects: 1 })) },
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
        f.status.mockResolvedValueOnce(residencyStatus()).mockResolvedValue(residencyStatus({ remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0 }))
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'cancelled', retryable: true })
        click(sync.disconnect); await settle()
        expect(f.disconnect).not.toHaveBeenCalled(); expect(f.release).toHaveBeenCalledOnce()
        expect(alertText()).toBeUndefined()
    })
    it('disconnects without asking when the remaining files are kept only in external storage', async () => {
        await mountBound(residencyStatus(externalOnly))
        click(sync.disconnect); await settle()
        expect(f.checkbox).not.toHaveBeenCalled(); expect(f.policy).not.toHaveBeenCalled()
        expect(f.disconnect).toHaveBeenCalledOnce()
    })
    it('disconnects when the failed download leaves files only in external storage', async () => {
        await mountBound()
        f.status.mockResolvedValueOnce(residencyStatus()).mockResolvedValueOnce(residencyStatus({ ...externalOnly, remoteObjects: 1 }))
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'required-asset-unavailable', retryable: false })
        click(sync.disconnect); await settle()
        expect(f.disconnect).toHaveBeenCalledOnce()
        expect(alertText()).toBeUndefined()
    })
    it('keeps the download failure when the status refresh afterwards fails', async () => {
        await mountBound()
        f.status.mockResolvedValueOnce(residencyStatus()).mockResolvedValueOnce(residencyStatus()).mockRejectedValueOnce({ code: 'server-unreachable' })
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'server-unreachable', retryable: true })
        click(sync.disconnect); await settle()
        expect(f.status).toHaveBeenCalledTimes(4)
        expect(f.disconnect).not.toHaveBeenCalled()
        expect(alertText()).toBe(sync.downloadFailedKeptConnection)
    })
    it('keeps the download failure when the first status read fails after it', async () => {
        f.view = { status: { configured: true, bound: true }, paused: false, error: '' }
        let failMount!: (error: unknown) => void
        f.status.mockResolvedValue(residencyStatus()).mockImplementationOnce(() => new Promise((_, reject) => { failMount = reject }))
        f.checkbox.mockResolvedValue({ confirmed: true, checked: true })
        f.policy.mockRejectedValue({ code: 'server-unreachable', retryable: true })
        component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
        await settle()
        click(sync.disconnect); await settle()
        expect(alertText()).toBe(sync.downloadFailedKeptConnection)
        failMount({ code: 'server-unreachable' }); await settle()
        expect(alertText()).toBe(sync.downloadFailedKeptConnection)
    })
    it('disconnects without asking when the status cannot be read', async () => {
        await mountBound()
        f.status.mockRejectedValueOnce(new Error('status-unavailable'))
        click(sync.disconnect); await settle()
        expect(f.checkbox).not.toHaveBeenCalled(); expect(f.policy).not.toHaveBeenCalled()
        expect(f.disconnect).toHaveBeenCalledOnce()
    })
})
it('removes the asset storage controls once the server is disconnected', async () => {
    await mountBound(residencyStatus({ policy: 'full', remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0 }))
    expect(host.textContent).toContain(sync.residency.title)
    f.disconnect.mockImplementation(async () => { publish({ status: { configured: true, bound: false }, paused: true, error: '' }) })
    click(sync.disconnect); await settle()
    expect(f.disconnect).toHaveBeenCalledOnce()
    expect(host.textContent).not.toContain(sync.residency.title)
    for (const label of [sync.residency.full, sync.residency.remote, sync.residency.clean, sync.residency.download]) expect(findButton(label)).toBeUndefined()
})
it('shows no asset storage controls for a stored server registration that is not connected', async () => {
    f.view = { status: { configured: true, bound: false }, paused: true, error: '' }
    f.status.mockResolvedValue(residencyStatus({ policy: 'full' }))
    component = mount(ServerSyncSettings, { target: host, props: { connectTarget: vi.fn() } })
    await settle()
    expect(host.textContent).not.toContain(sync.residency.title)
    expect(findButton(sync.residency.download)).toBeUndefined()
})
it('counts only files the server holds as kept only on the server', async () => {
    await mountBound(residencyStatus({ remoteBytes: 8 * 1024 * 1024, serverBytes: 4 * 1024 * 1024 }))
    const row = [...host.querySelectorAll('*')].find(node => node.children.length === 0 && node.textContent === sync.residency.remoteOnly)?.parentElement?.parentElement
    expect(row?.textContent).toContain('4.0 MiB')
    expect(host.textContent).not.toContain('8.0 MiB')
})
describe('downloading files kept only on the server', () => {
    it.each([
        { name: 'files only on the server', policy: 'full', status: {}, shown: true },
        { name: 'files only in external storage', policy: 'full', status: { ...externalOnly, remoteBytes: 4096 }, shown: true },
        { name: 'no remote files', policy: 'full', status: { remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0 }, shown: false },
        { name: 'files only on the server', policy: 'remote', status: {}, shown: false },
    ])('offers the download for policy $policy with $name: $shown', async ({ policy, status, shown }) => {
        await mountBound(residencyStatus({ policy, ...status }))
        expect(!!findButton(sync.residency.download)).toBe(shown)
    })
    it('shows the files kept only in external storage as their own row', async () => {
        await mountBound(residencyStatus({ policy: 'full', ...externalOnly, remoteBytes: 3 * 1024 * 1024 }))
        const row = [...host.querySelectorAll('*')].find(node => node.children.length === 0 && node.textContent === sync.residency.externalOnly)?.parentElement?.parentElement
        expect(row?.textContent).toContain('3.0 MiB')
        expect(findButton(sync.residency.download)).toBeDefined()
    })
    it('shows no external storage row when every remote file is on the server', async () => {
        await mountBound(residencyStatus({ policy: 'full' }))
        expect(host.textContent).not.toContain(sync.residency.externalOnly)
    })
    it('downloads with sync stopped and stays connected', async () => {
        await mountBound(residencyStatus({ policy: 'full' }))
        f.policy.mockResolvedValue(residencyStatus({ policy: 'full', remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0 }))
        f.status.mockResolvedValue(residencyStatus({ policy: 'full', remoteObjects: 0, remoteBytes: 0, serverObjects: 0, serverBytes: 0 }))
        click(sync.residency.download); await settle()
        expect(f.policy).toHaveBeenCalledExactlyOnceWith('full'); expect(f.disconnect).not.toHaveBeenCalled()
        const order = [f.hold, f.policy, f.release].map(mock => mock.mock.invocationCallOrder[0])
        expect(order).toEqual([...order].sort((a, b) => a - b))
        expect(findButton(sync.residency.download)).toBeUndefined()
    })
})

const onboarding = languageEnglish.risuNest.onboarding
const heading = () => host.querySelector('h1')?.textContent
function pick(title: string) { const entry = [...host.querySelectorAll('button')].find(button => button.querySelector('b')?.textContent === title); expect(entry).toBeDefined(); entry!.click() }
async function openOnboardingServer(expectConnectionScreen = true) {
    component = mount(Onboarding, { target: host }); await tick()
    pick(onboarding.home.syncTitle); await tick()
    pick(onboarding.sync.hubTitle); await tick()
    if (expectConnectionScreen) expect(heading()).toBe(onboarding.hub.title)
}
const registered = { endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }
it.each(['bound', 'cancelled'])('mounted onboarding advances only after %s shared binding outcome', async kind => {
    f.connect.mockResolvedValue({ kind })
    await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.connect); await settle()
    expect(f.connect).toHaveBeenCalledExactlyOnceWith(registered, false)
    expect(f.configure).not.toHaveBeenCalled(); expect(f.bind).not.toHaveBeenCalled()
    expect(heading()).toBe(kind === 'bound' ? onboarding.done.title : onboarding.hub.title)
})
it('mounted onboarding connects as a new device through the same action', async () => {
    f.view = { status: { configured: true, bound: false }, paused: true, error: 'writer-collision' }
    f.connect.mockResolvedValue({ kind: 'bound' })
    await openOnboardingServer(); await registration(); click(languageEnglish.lwwSync.newDeviceAction); await settle()
    expect(f.connect).toHaveBeenCalledExactlyOnceWith(registered, true)
    expect(heading()).toBe(onboarding.done.title)
})
it.each([false, true])('mounted onboarding mentions the QR code only where it can be scanned (%s)', async scan => {
    f.scan = scan
    await openOnboardingServer()
    expect(host.textContent).toContain(scan ? onboarding.hub.leadScan : onboarding.hub.lead)
    expect(host.textContent?.includes('QR')).toBe(scan)
})
describe('onboarding on a device already connected to the server', () => {
    const connected = { status: { configured: true, bound: true }, paused: false, error: '' }
    it('finishes without connecting again', async () => {
        f.view = connected
        await openOnboardingServer(false); await settle()
        expect(heading()).toBe(onboarding.done.title)
        expect(host.textContent).toContain(onboarding.done.data)
        expect(f.connect).not.toHaveBeenCalled(); expect(f.bind).not.toHaveBeenCalled()
    })
    it('stays on the connection screen while the connection is unfinished, and finishes once it completes', async () => {
        f.view = { status: { configured: false, bound: true }, paused: true, error: '', bindingIncomplete: true }
        await openOnboardingServer(); await settle()
        expect(heading()).toBe(onboarding.hub.title)
        expect(host.querySelector('[role="status"]')?.textContent).toBe(languageEnglish.lwwSync.bindingIncomplete)
        publish(connected); await settle()
        expect(heading()).toBe(onboarding.done.title)
    })
    it('keeps a failed connection that left the device connected on screen until sync runs', async () => {
        f.connect.mockImplementation(async () => {
            publish({ status: { configured: true, bound: true }, paused: true, error: 'server-unreachable' })
            throw Object.assign(new Error('server-unreachable'), { code: 'server-unreachable' })
        })
        await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.connect); await settle()
        expect(heading()).toBe(onboarding.hub.title)
        expect(host.querySelector('[role="alert"]')).not.toBeNull()
        publish(connected); await settle()
        expect(heading()).toBe(onboarding.done.title)
    })
})
describe('starting fresh from onboarding', () => {
    const startingValues = ['preset', 'textTheme', 'maxContext', 'maxResponse', 'claudeCachingExperimental']
    it('applies the starting settings to an empty library that is not connected', async () => {
        component = mount(Onboarding, { target: host }); await tick()
        pick(onboarding.home.freshTitle); await settle()
        expect(f.preset).toHaveBeenCalledOnce()
        expect(f.state.db).toMatchObject({ preset: 'starting', textTheme: 'highcontrast', maxContext: 16000, maxResponse: 1000, claudeCachingExperimental: true })
        expect(heading()).toBe(onboarding.done.title)
        expect(host.textContent).toContain(onboarding.done.fresh)
    })
    it.each([
        { name: 'connected to a sync server', binding: { target: { kind: 'server', connectionId: 'server' } }, characters: [], summary: onboarding.done.data },
        { name: 'connected to external storage', binding: { target: { kind: 'external', connectionId: 'external' } }, characters: [], summary: onboarding.done.data },
        { name: 'holding characters', binding: { target: { kind: 'none' } }, characters: [{ chaId: 'synthetic' }], summary: onboarding.done.data },
    ])('keeps the shared settings of a library $name', async ({ binding, characters, summary }) => {
        f.bindingState.mockResolvedValue(binding)
        f.state.db.characters = characters
        component = mount(Onboarding, { target: host }); await tick()
        pick(onboarding.home.freshTitle); await settle()
        expect(f.preset).not.toHaveBeenCalled()
        for (const key of startingValues) expect(f.state.db).not.toHaveProperty(key)
        expect(heading()).toBe(onboarding.done.title)
        expect(host.textContent).toContain(summary)
    })
    it('keeps the shared settings when the connection state cannot be read', async () => {
        f.bindingState.mockRejectedValue(new Error('binding-state-unavailable'))
        component = mount(Onboarding, { target: host }); await tick()
        pick(onboarding.home.freshTitle); await settle()
        expect(f.preset).not.toHaveBeenCalled()
        for (const key of startingValues) expect(f.state.db).not.toHaveProperty(key)
        expect(heading()).toBe(onboarding.done.title)
        expect(host.textContent).toContain(onboarding.done.data)
        expect(host.textContent).not.toContain(onboarding.done.import)
    })
})
it('mounted web onboarding offers no native server entry', async () => {
    f.native = false; component = mount(Onboarding, { target: host }); await tick()
    expect(host.textContent).not.toContain(languageEnglish.risuNest.onboarding.sync.hubTitle)
    expect(host.querySelector('#server-registration')).toBeNull()
})
it('mounted onboarding discards the registration without advancing', async () => {
    await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.discardRegistration); await settle()
    expect(f.connect).not.toHaveBeenCalled()
    expect(host.querySelector('h1')?.textContent).toBe(languageEnglish.risuNest.onboarding.hub.title)
})

it('mounted onboarding keeps binding failures on the connection screen', async () => {
    f.connect.mockRejectedValue(new Error('refresh-failed'))
    await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.connect); await settle()
    expect(host.querySelector('h1')?.textContent).toBe(languageEnglish.risuNest.onboarding.hub.title)
    expect(host.querySelector('[role="alert"]')).not.toBeNull()
})
it('a late bound response does not advance a dismissed server screen', async () => {
    let finish!: (value: { kind: string }) => void
    f.connect.mockImplementation(() => new Promise(resolve => { finish = resolve }))
    await openOnboardingServer(); await registration(); click(languageEnglish.risuNest.serverSync.connect); await settle()
    click(languageEnglish.risuNest.onboarding.back); await tick()
    finish({ kind: 'bound' }); await settle()
    expect(host.querySelector('h1')?.textContent).toBe(languageEnglish.risuNest.onboarding.sync.title)
})
