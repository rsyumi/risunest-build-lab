// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const m = vi.hoisted(() => ({
    native: true,
    export: vi.fn(), restore: vi.fn(), webExport: vi.fn(), webImport: vi.fn(),
    confirm: vi.fn(), normal: vi.fn(), error: vi.fn(),
}))
vi.mock('src/ts/platform', () => ({ get isTauri() { return m.native } }))
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: {} } }))
vi.mock('src/ts/characterCards', () => ({ hubURL: 'https://synthetic.invalid' }))
vi.mock('src/ts/storage/deviceMarkers', () => ({ getDeviceMarkers: vi.fn() }))
vi.mock('src/ts/drive/accounter', () => ({ loadRisuAccountBackup: vi.fn() }))
vi.mock('src/ts/alert', () => ({ alertConfirm: m.confirm, alertCheckboxConfirm: async () => ({ confirmed: await m.confirm(), checked: true }), alertNormal: m.normal, alertError: m.error, openRisuAccountLogin: vi.fn() }))
vi.mock('src/ts/globalApi.svelte', () => ({ forageStorage: { isAccount: false } }))
vi.mock('src/ts/storage/dataHealthNavigation', () => ({ openDataHealthScreen: vi.fn() }))
vi.mock('src/ts/storage/accountStorage', async () => ({
    unMigrationAccount: vi.fn(), accountUnmigrationBusy: (await import('svelte/store')).writable(false),
}))
vi.mock('src/ts/drive/backuplocal', () => ({ SaveLocalBackup: m.webExport, LoadLocalBackup: m.webImport, SavePartialLocalBackup: vi.fn() }))
vi.mock('src/ts/storage/exportAsDataset', () => ({ exportAsDataset: vi.fn() }))
vi.mock('src/ts/sionyw', () => ({ loginToSionyw: vi.fn(), testSionywLogin: vi.fn() }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountFlow', () => ({ getNativeOfficialAccountFlow: vi.fn(), NativeAccountLoginError: class extends Error {} }))
vi.mock('src/ts/storage/portableBackupFileRouteProduction.svelte', () => ({ exportPortableBackupFromSystemPicker: m.export, restoreBackupFromSystemPicker: m.restore }))
vi.mock('src/ts/storage/risuSaveFileRouteProduction.svelte', async () => ({ nativeFileOperation: (await import('svelte/store')).writable(null), exportRisuSaveFromSystemPicker: vi.fn() }))
vi.mock('src/ts/storage/risuSaveFileRoute', () => ({ alertPartialDestinationWarning: vi.fn(), hasPartialDestinationWarning: () => false }))
vi.mock('src/ts/storage/nativeFileJobManager', async () => ({
    NativeFileOperationBusyError: class extends Error {},
    nativeFileOperationOutcome: (await import('svelte/store')).writable(null),
    dismissNativeFileOperationOutcome: vi.fn(), nativeFileOperationOutcomeShown: () => false,
}))
vi.mock('src/ts/storage/compatibleBackupFileRouteProduction.svelte', () => ({ exportCompatibilityBackupFromSystemPicker: vi.fn() }))

import { language } from 'src/lang'
import { nativeFileOperation } from 'src/ts/storage/risuSaveFileRouteProduction.svelte'
import { nativeFileOperationOutcome } from 'src/ts/storage/nativeFileJobManager'
import UserSettings from './UserSettings.svelte'

let mounted: ReturnType<typeof mount> | undefined
let target: HTMLDivElement
beforeEach(() => {
    vi.resetAllMocks()
    m.native = true
    m.confirm.mockResolvedValue(true)
    nativeFileOperation.set(null)
    nativeFileOperationOutcome.set(null)
    target = document.createElement('div')
    document.body.append(target)
})
afterEach(async () => {
    if (mounted) await unmount(mounted)
    mounted = undefined
    document.body.replaceChildren()
})
async function render(native = true) {
    m.native = native
    mounted = mount(UserSettings, { target })
    await tick()
}
function button(label: string) {
    const match = [...target.querySelectorAll('button')].find((node) => node.textContent?.trim() === label)
    expect(match, label).toBeDefined()
    return match!
}
async function click(label: string) {
    button(label).click()
    await tick()
    await Promise.resolve()
    await tick()
}

describe('rendered backup actions', () => {
    it('calls native export once and leaves success reporting to its dialog', async () => {
        m.export.mockResolvedValue({ warningCodes: [] })
        await render()
        await click(language.portableBackup.export)
        expect(m.export).toHaveBeenCalledOnce()
        expect(m.webExport).not.toHaveBeenCalled()
        expect(m.normal).not.toHaveBeenCalled()
    })
    it('reports neither success nor failure when native restore is cancelled', async () => {
        m.restore.mockResolvedValue(null)
        await render()
        await click(language.portableBackup.restore)
        expect(m.restore).toHaveBeenCalledOnce()
        expect(m.confirm).not.toHaveBeenCalled()
        expect(m.normal).not.toHaveBeenCalled()
        expect(m.error).not.toHaveBeenCalled()
    })
    it('shows the cleanup warning after a completed native restore', async () => {
        m.restore.mockResolvedValue({ warningCodes: ['cleanup-failed'] })
        await render()
        await click(language.portableBackup.restore)
        expect(m.normal).toHaveBeenCalledWith(`${language.risuNest.backup.localBackupRestored} ${language.risuSaveCleanupWarning}`)
    })
    it('lets the WebView importer preflight and acknowledge the selected backup', async () => {
        await render(false)
        await click(language.loadBackupLocal)
        expect(m.webImport).toHaveBeenCalledOnce()
        expect(m.confirm).not.toHaveBeenCalled()
        expect(m.restore).not.toHaveBeenCalled()
        expect(m.normal).not.toHaveBeenCalled()
    })
    it('uses the web export adapter', async () => {
        await render(false)
        await click(language.saveBackupLocal)
        expect(m.webExport).toHaveBeenCalledOnce()
        expect(m.export).not.toHaveBeenCalled()
    })
    it('reports a rejected native picker through the error path', async () => {
        m.restore.mockRejectedValue(new Error('synthetic picker failure'))
        await render()
        await click(language.portableBackup.restore)
        expect(m.error).toHaveBeenCalledOnce()
        expect(m.error).toHaveBeenCalledWith(language.risuNest.backup.actionFailed)
        expect(m.normal).not.toHaveBeenCalled()
    })
    it('keeps buttons disabled while the shared operation is busy, then permits cancellation', async () => {
        let finish!: (value: null) => void
        m.restore.mockImplementation(() => {
            nativeFileOperation.set({ kind: 'import' } as never)
            return new Promise<null>((resolve) => { finish = resolve })
        })
        await render()
        await click(language.portableBackup.restore)
        expect(button(language.portableBackup.restore).disabled).toBe(true)
        expect(button(language.portableBackup.export).disabled).toBe(true)
        button(language.portableBackup.restore).click()
        expect(m.restore).toHaveBeenCalledOnce()
        finish(null)
        nativeFileOperation.set(null)
        await tick()
        expect(button(language.portableBackup.restore).disabled).toBe(false)
        expect(m.normal).not.toHaveBeenCalled()
    })
    it.each([['dialog', false], ['inline', true]] as const)('shows the busy notice for a running %s operation: %s', async (presentation, shown) => {
        nativeFileOperation.set({ kind: 'export', presentation } as never)
        await render()
        expect(target.textContent?.includes(language.risuNest.backup.fileBusy)).toBe(shown)
        expect(button(language.portableBackup.export).disabled).toBe(true)
    })
})
