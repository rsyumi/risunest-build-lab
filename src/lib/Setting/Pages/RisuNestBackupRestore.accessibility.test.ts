// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const platform = vi.hoisted(() => ({ native: false, android: false, desktop: false }))

vi.mock('src/ts/platform', async (importOriginal) => ({
    ...(await importOriginal<typeof import('src/ts/platform')>()),
    get isTauri() { return platform.native },
    get isTauriAndroid() { return platform.android },
    get isTauriDesktop() { return platform.desktop },
}))
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: { account: undefined } } }))
vi.mock('src/ts/alert', () => ({
    alertConfirm: vi.fn(),
    alertError: vi.fn(),
    alertNormal: vi.fn(),
    alertSelect: vi.fn(),
}))
vi.mock('src/ts/drive/backuplocal', () => ({ LoadLocalBackup: vi.fn() }))
vi.mock('src/ts/storage/sync/syncConflictRestore', () => ({ openSyncConflictBackups: vi.fn() }))
vi.mock('src/ts/storage/nativePersistentMaintenance', () => ({
    restoreNativePersistentSnapshot: vi.fn(),
    restartNativeApp: vi.fn(),
}))
vi.mock('src/ts/storage/sync/nativeOfficialAccountOperations', () => ({
    restoreNativeOfficialAccountBackup: vi.fn(), publishNativeOfficialAccountBackup: vi.fn(),
}))
vi.mock('src/ts/storage/fileOperationErrorPresentation', () => ({ presentFileOperationError: vi.fn() }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountFlow', () => ({
    getNativeOfficialAccountFlow: vi.fn(),
}))
vi.mock('src/ts/storage/risuSaveFileRouteProduction.svelte', async () => {
    const { writable } = await import('svelte/store')
    return {
        nativeFileOperation: writable({
            kind: 'export',
            presentation: 'inline',
            status: { stage: 'writing', completedUnits: 1, totalUnits: 2 },
        }),
        importRisuSaveFromSystemPicker: vi.fn(),
        exportRisuSaveFromSystemPicker: vi.fn(),
    }
})
vi.mock('src/ts/storage/portableBackupFileRouteProduction.svelte', () => ({
    restoreBackupFromSystemPicker: vi.fn(),
}))
vi.mock('src/ts/storage/risuSaveFileRoute', () => ({
    alertPartialDestinationWarning: vi.fn(),
    hasPartialDestinationWarning: vi.fn(() => false),
}))
vi.mock('src/ts/storage/nativeFileJobManager', () => ({
    cancelActiveNativeFileOperation: vi.fn(),
    dismissNativeFileOperationOutcome: vi.fn(),
    nativeFileOperationOutcomeShown: vi.fn(() => false),
}))
vi.mock('src/ts/gui/nativeFileJobProgress', () => ({
    nativeFileJobProgressText: vi.fn(() => 'Writing backup'),
    nativeFileJobTitle: vi.fn(() => 'Export RisuSave'),
}))
vi.mock('src/ts/storage/sync/external/bridge', () => ({
    getExternalStorageBridge: () => ({
        getState: vi.fn(async () => ({
            supported: false,
            selection: {
                kind: 'none',
                selectionEpoch: 'test-selection',
                paused: false,
                decisionRequired: false,
            },
            connections: [],
            jobs: [],
        })),
    }),
}))
vi.mock('src/ts/storage/sync/external/production', () => ({
    refreshExternalStorageProductionState: vi.fn(),
    requestExternalStorageNow: vi.fn(),
    requestExternalStorageRestore: vi.fn(),
}))

import { language } from 'src/lang'
import { nativeFileOperation } from 'src/ts/storage/risuSaveFileRouteProduction.svelte'
import { restoreBackupFromSystemPicker } from 'src/ts/storage/portableBackupFileRouteProduction.svelte'
import { LoadLocalBackup } from 'src/ts/drive/backuplocal'
import { alertConfirm } from 'src/ts/alert'
import RisuNestBackupRestore from './RisuNestBackupRestore.svelte'

let mounted: ReturnType<typeof mount> | undefined

beforeEach(() => { platform.native = false; platform.android = false; platform.desktop = false; vi.clearAllMocks() })

afterEach(async () => {
    if (mounted) await unmount(mounted)
    mounted = undefined
    document.body.replaceChildren()
})

it('announces inline backup progress through a polite status region', async () => {
    const target = document.createElement('div')
    document.body.append(target)
    mounted = mount(RisuNestBackupRestore, { target })
    await tick()

    const status = target.querySelector('[role="status"]')
    expect(status?.getAttribute('aria-live')).toBe('polite')
    expect(status?.textContent).toContain('Writing backup')
})

it.each(['desktop', 'android', 'ios'])('offers one generic native import on %s', async targetPlatform => {
    platform.native = true
    platform.android = targetPlatform === 'android'
    platform.desktop = targetPlatform === 'desktop'
    nativeFileOperation.set(null)
    const target = document.createElement('div')
    document.body.append(target)
    mounted = mount(RisuNestBackupRestore, { target })
    await tick()
    const buttons = [...target.querySelectorAll('button')]
    const imports = buttons.filter(button => button.textContent?.trim() === language.risuNest.backup.importFile)
    expect(imports).toHaveLength(1)
    expect(buttons.some(button => button.textContent?.includes(language.loadPocketRisuBackup))).toBe(false)
    imports[0].click()
    await tick()
    expect(restoreBackupFromSystemPicker).toHaveBeenCalledOnce()
    expect(LoadLocalBackup).not.toHaveBeenCalled()
})

it('preserves the distinct web PocketRisu importer', async () => {
    nativeFileOperation.set(null)
    vi.mocked(alertConfirm).mockResolvedValue(true)
    const target = document.createElement('div')
    document.body.append(target)
    mounted = mount(RisuNestBackupRestore, { target })
    await tick()
    const button = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === language.loadPocketRisuBackup)!
    button.click()
    await vi.waitFor(() => expect(LoadLocalBackup).toHaveBeenCalledOnce())
    expect(restoreBackupFromSystemPicker).not.toHaveBeenCalled()
})
