import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const startupMocks = vi.hoisted(() => ({ checkNativeStartupStatus: vi.fn() }))
const startup = vi.hoisted(() => ({
    native: true, exclusions: [] as string[], database: {} as any, reachUi: false,
    takeRendererRecovery: vi.fn(async () => false),
    credential: { id: 'synthetic-account', token: 'synthetic-token' },
    readVault: vi.fn(), reconcile: vi.fn(), external: vi.fn(), transports: vi.fn(), stages: vi.fn(),
    serverPaused: true, lwwRunning: false, exitDrain: vi.fn(), serverDrain: vi.fn(),
    stop: new Error('synthetic bootstrap test reached UI'),
    markers: { getItem: vi.fn(() => null), setItem: vi.fn(), removeItem: vi.fn(), flush: vi.fn(async () => {}) },
}))

vi.mock('svelte/store', async (importOriginal) => ({
    ...await importOriginal<typeof import('svelte/store')>(),
    get: vi.fn(() => false),
}))
vi.mock('@tauri-apps/plugin-fs', () => ({
    BaseDirectory: { AppData: 'app-data' },
    exists: vi.fn(), mkdir: vi.fn(), readDir: vi.fn(), readFile: vi.fn(),
    remove: vi.fn(), writeFile: vi.fn(),
}))
vi.mock('@tauri-apps/api/webviewWindow', () => ({ getCurrentWebviewWindow: () => ({ maximize: vi.fn() }) }))
vi.mock('@tauri-apps/api/core', () => ({ convertFileSrc: vi.fn() }))
vi.mock('@tauri-apps/api/path', () => ({ join: vi.fn() }))
vi.mock('./util', () => ({ changeFullscreen: vi.fn(), sleep: vi.fn() }))
vi.mock('./update', () => ({ checkRisuUpdate: vi.fn() }))
vi.mock('./gui/animation', () => ({ updateAnimationSpeed: vi.fn() }))
vi.mock('./gui/colorscheme', () => ({
    updateColorScheme: () => { if (!startup.reachUi) throw startup.stop }, updateTextThemeAndCSS: vi.fn(),
}))
vi.mock('./observer.svelte', () => ({ startObserveDom: vi.fn() }))
vi.mock('./gui/guisize', () => ({ updateGuisize: vi.fn() }))
vi.mock('./hotkey', () => ({ initMobileGesture: vi.fn() }))
vi.mock('./process/modules', () => ({ moduleUpdate: vi.fn() }))
vi.mock('./drive/accounter', () => ({ loadRisuAccountData: vi.fn() }))
vi.mock('./storage/risuSave', () => ({ decodeRisuSave: vi.fn() }))
vi.mock('./storage/remoteSaveCleanup', () => ({ getRemoteSaveCleanupAction: vi.fn(), getRemoteSavePayloadName: vi.fn() }))
vi.mock('./model/modellist', () => ({ registerModelDynamic: vi.fn() }))
vi.mock('./nativeScreenshotArchiveWriter', () => ({
    describeScreenshotPublicationError: vi.fn(), listenRecoveredAndroidScreenshotPublications: vi.fn(),
}))
vi.mock('./storage/nativePersistentMaintenance', () => ({
    restartNativeApp: vi.fn(), schedulePeriodicNativeSnapshot: vi.fn(),
}))
vi.mock('./storage/nativeFileJobs', () => ({
    NativeFileJobError: class extends Error {}, runNativeOfficialAccountSnapshotRestore: vi.fn(),
}))
vi.mock('./storage/androidRisuSaveRouteProduction.svelte', () => ({ registerAndroidRisuSaveRoute: vi.fn() }))
vi.mock('./storage/iosFiles', () => ({ reportInterruptedIOSBackupSourcesAtStart: vi.fn() }))
vi.mock('src/lang', () => ({ changeLanguage: vi.fn(), language: { risuNest: {
    startup: {
        storage: 'Storage', account: 'Account', plugins: 'Plugins', ui: 'UI', data: 'Data', serviceWorker: 'Service worker',
        rendererRecovered: 'Synthetic recovery notice',
    },
    backup: { officialAssetsRestoreFailed: 'Synthetic asset restore failed', officialAssetsMissing: 'Synthetic missing assets: {count}' },
} } }))
vi.mock('./platform', () => ({ get isTauri() { return startup.native }, isTauriAndroid: false, isTauriDesktop: false }))
vi.mock('./storage/deviceSettings', () => ({ getStartupExclusions: () => startup.exclusions, loadDeviceSettings: () => ({ nativeFileLogEnabled: false }) }))
vi.mock('./nativeLog', () => ({ recordNativeLogError: vi.fn(), setNativeLogFileEnabled: vi.fn() }))
vi.mock('./storage/persistentStorageRuntime', () => ({
    initializePersistentStorage: vi.fn(), activateNativeAssetRepository: vi.fn(),
}))
vi.mock('./storage/nativeFileJobRecovery', () => ({
    shouldReconcileNativeFileJobs: vi.fn(() => false),
    reconcileNativeFileJobsBeforeBootstrap: async () => ({ pendingRestoreAcknowledgements: [], pendingOfficialPublications: [], interruptedRestores: [] }),
    acknowledgeRecoveredNativeRestores: vi.fn(),
}))
vi.mock('./storage/persistentBootstrap', () => ({ bootstrapPersistentDatabase: async () => ({ database: startup.database, revision: 1, source: 'persistent' }) }))
vi.mock('./storage/database.svelte', () => ({ setDatabase: (value: unknown) => { startup.database = value }, getDatabase: () => startup.database }))
vi.mock('./storage/databasePreparation', () => ({
    checkNewFormat: vi.fn(), prepareDatabaseForPersistence: vi.fn(), prepareDatabaseForBootstrap: vi.fn(), preparePersistentRootForWorkingSet: vi.fn(),
    assignIds: vi.fn(),
}))
vi.mock('./storage/workingSetCatalog', () => ({
    createCatalogPresetWorkingSet: vi.fn(), hasIncompletePersistentWorkingSet: vi.fn(() => false),
    isCatalogCharacterStub: vi.fn(() => false), isCatalogPresetWorkingSet: vi.fn(() => false),
    isWorkingSetCharacterStub: vi.fn(() => false),
    projectCatalogWorkingSet: vi.fn(), projectCompleteScalableWorkingSet: vi.fn(),
}))
vi.mock('./storage/workingSetResidency', () => ({
    workingSetResidency: { clear: vi.fn(), markCharacterReleased: vi.fn(), reconcileConversationResidency: vi.fn() },
}))
vi.mock('./storage/persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: () => ({ store: {}, revision: 1, flushPendingData: vi.fn(), expirePersistentTrash: vi.fn(async () => {}),
        capturePersistentMutationToken: vi.fn(async () => ({ revision: 5 })), acquireDestructiveReplacementFence: vi.fn(async () => ({ revision: 5 })) }),
    initializeActiveWorkingSet: vi.fn(), configurePersistentDataRuntime: vi.fn(),
    hasPendingOfficialPublication: vi.fn(() => false), publishCurrentOfficialRevision: vi.fn(),
}))
vi.mock('./plugins/plugins.svelte', () => ({
    loadPlugins: vi.fn(), pluginCompatibility: { initialize: vi.fn(), profile: 'default' },
}))
vi.mock('./plugins/pluginCompatibility', () => ({ shouldProjectScalableWorkingSet: vi.fn(() => false) }))
vi.mock('./storage/accountStorage', () => ({
    AccountStorage: class { readItem = vi.fn() }, resetAccountStorageSession: vi.fn(),
}))
vi.mock('./storage/nativeAccountCredential', () => ({
    createNativeAccountCredentialVault: () => ({ read: startup.readVault }),
}))
vi.mock('./storage/nativeDeviceSettings', () => ({
    createNativeDeviceSettings: () => ({ get: vi.fn(async () => null), set: vi.fn() }), createNativeDeviceSettingsBag: vi.fn(),
}))
vi.mock('./storage/sync/officialAccountSnapshot', () => ({
    OfficialAccountSnapshotAdapter: class {}, createOfficialAssociationMarkers: () => ({}),
}))
vi.mock('./storage/sync/officialAccountBootstrap', () => ({
    initializeOfficialAccountBootstrap: vi.fn(async () => ({ officialEnabled: false })), publishOfficialRevisionIfChanged: vi.fn(),
}))
vi.mock('./storage/sync/officialAssetLedger', () => ({
    createAccountScopedOfficialAssetLedger: () => ({ reset: vi.fn() }),
}))
vi.mock('./storage/accountAssetAccess', () => ({
    configureOfficialAccountAssetReader: vi.fn(), createStructuredAccountAssetReader: vi.fn(),
}))
vi.mock('./storage/sync/nativeOfficialAccountFlow', () => ({
    configureNativeOfficialAccountFlow: vi.fn(), createNativeOfficialAccountFlowService: () => ({ flow: {}, snapshotRequestReauthentication: {} }),
    nativeOfficialAccountKeys: { credential: 'credential', association: 'association', assetLedger: 'asset-ledger' },
    normalizeNativeOfficialAccountCredential: (value: unknown) => value,
}))
vi.mock('./storage/sync/nativeOfficialPublicationJob', () => ({
    createNativeOfficialPublicationJobPublisher: vi.fn(() => ({})),
}))
vi.mock('./storage/sync/nativeOfficialPublicationRecovery', () => ({
    createNativeOfficialPublicationRecovery: () => ({ reconcile: startup.reconcile, reconcileSettled: startup.reconcile }),
}))
vi.mock('./storage/platformBlobStore', () => ({ resolveBlobStore: vi.fn() }))
vi.mock('./storage/sync/syncConflictBackup', () => ({ getSyncConflictBackupStore: vi.fn() }))
vi.mock('./storage/sync/syncConflictSummary', () => ({ formatNameList: vi.fn(), summarizePinnedSyncConflict: vi.fn() }))
vi.mock('./storage/persistentRecordIterator', () => ({ withPersistentRevisionLease: vi.fn() }))
vi.mock('./process/coldstorage.svelte', () => ({
    getAccountColdStorageItem: vi.fn(), getColdStorageItem: vi.fn(), makeColdData: vi.fn(),
    setAccountColdStorageItem: vi.fn(),
}))
vi.mock('./globalApi.svelte', () => ({
    forageStorage: { Init: vi.fn(async () => {}), isAccount: false, setAccountModeForSession: vi.fn() }, saveDb: vi.fn(),
    getUncleanables: vi.fn(), getBasename: vi.fn(), invalidateAssetSourceCache: vi.fn(), setUsingSw: vi.fn(),
}))
vi.mock('./stores.svelte', () => ({
    MobileGUI: { set: vi.fn() }, botMakerMode: { set: vi.fn() }, selectedCharID: { set: vi.fn() },
    loadedStore: { set: vi.fn() }, DBState: {}, LoadingStatusState: { text: '' }, bootFailure: { set: vi.fn() },
}))
vi.mock('./alert', () => ({
    alertConfirm: vi.fn(), alertError: vi.fn(), alertInput: vi.fn(), alertLogin: vi.fn(), alertMd: vi.fn(),
    alertNormal: vi.fn(), alertSelect: vi.fn(), alertTOS: vi.fn(), alertRisuServiceTOS: vi.fn(), waitAlert: vi.fn(),
    alertToast: vi.fn(),
}))
vi.mock('./characterCards', () => ({ applyHubSelection: vi.fn(), characterURLImport: vi.fn(), hubURL: 'https://hub.invalid' }))
vi.mock('./storage/androidSafBridge', () => ({ isAndroidSafFileJobsEnabled: vi.fn(() => false) }))
vi.mock('./storage/lifecycleCommit', () => ({ registerLifecycleCommitListeners: vi.fn() }))
vi.mock('./nativeStartup', () => startupMocks)


vi.mock('./storage/deviceMarkers', () => ({ initializeDeviceMarkers: vi.fn(), getDeviceMarkers: () => startup.markers }))
vi.mock('./storage/recoveryMode.svelte', async (importOriginal) => ({
    ...await importOriginal<typeof import('./storage/recoveryMode.svelte')>(),
    finishBoot: vi.fn(async () => {}),
    takeRendererRecovery: startup.takeRendererRecovery,
}))
vi.mock('./storage/bootAttempt', () => ({ markBootStage: startup.stages, markBootSuspect: vi.fn() }))
vi.mock('./ui/yieldToUi', () => ({ yieldToUi: async () => {} }))
vi.mock('./nativeLocalUrls', () => ({ initializeNativeLocalUrls: vi.fn() }))
vi.mock('./iosNative', () => ({ initializeIOSNative: vi.fn(), installIOSPersistenceLifecycle: vi.fn() }))
vi.mock('./storage/sync/external/production', () => ({
    installExternalStorageProduction: startup.external, installExternalSyncTransports: startup.transports,
    getExternalStorageSyncExitDrainAdapter: startup.exitDrain,
}))
vi.mock('./storage/sync/external/lwwProduction', () => ({ isExternalLwwRunning: () => startup.lwwRunning }))
vi.mock('./storage/syncExitCoordinator', () => ({ createSyncExitCoordinator: vi.fn(() => ({})) }))
vi.mock('./storage/syncExitProduction', () => ({ configureSyncExitCoordinator: vi.fn(), registerWindowCloseDrain: vi.fn() }))
vi.mock('./storage/sync/serverSyncProduction', () => ({
    createServerSyncExitDrainAdapter: startup.serverDrain, initializeNativeSyncBindings: vi.fn(),
    getServerSyncController: () => ({ snapshot: () => ({ paused: startup.serverPaused }) }),
    installServerSyncProduction: vi.fn(), disposeNativeSyncBindings: vi.fn(),
}))
vi.mock('./storage/sync/external/bridge', () => ({ getExternalStorageBridge: vi.fn() }))
vi.mock('./process/transformers', () => ({ releaseIdleTransformerModels: vi.fn() }))
vi.mock('./process/files/inlayProviderImage', () => ({ forgetInlayProviderImages: vi.fn() }))

import { loadData } from './bootstrap'
import { alertNormal, alertToast, alertTOS, waitAlert } from './alert'
import { bootFailure } from './stores.svelte'
import { loadRisuAccountData } from './drive/accounter'
import { initializeOfficialAccountBootstrap } from './storage/sync/officialAccountBootstrap'
import { initializeNativeSyncBindings, installServerSyncProduction } from './storage/sync/serverSyncProduction'
import { createSyncExitCoordinator } from './storage/syncExitCoordinator'
import { getExternalStorageBridge } from './storage/sync/external/bridge'

beforeEach(() => {
    vi.clearAllMocks()
    startup.reachUi = false
    vi.stubGlobal('localStorage', { getItem: () => null, setItem: vi.fn(), removeItem: vi.fn() })
    vi.stubGlobal('navigator', {})
    startup.database = { characters: [], botPresets: [], account: { id: 'web-account', token: 'synthetic-web-token' } }
    startup.readVault.mockResolvedValue(startup.credential)
    vi.spyOn(console, 'error').mockImplementation(() => undefined)
})

afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
})

describe('bootstrap recovery exclusions', () => {
    it.each([true, false])('skips account and sync startup work while reaching UI (native=%s)', async (native) => {
        startup.native = native
        startup.exclusions = ['account', 'sync']
        await loadData()
        expect(bootFailure.set).toHaveBeenLastCalledWith(expect.objectContaining({
            stage: 'ui-state', message: startup.stop.message,
        }))
        expect(initializeOfficialAccountBootstrap).not.toHaveBeenCalled()
        expect(loadRisuAccountData).not.toHaveBeenCalled()
        expect(startup.reconcile).not.toHaveBeenCalled()
        expect(startup.external).not.toHaveBeenCalled()
        expect(startup.stages.mock.calls.flat()).not.toContain('account-bootstrap')
        expect(startup.stages.mock.calls.flat()).not.toContain('service-worker')
        if (native) {
            expect(initializeNativeSyncBindings).toHaveBeenCalledOnce()
            // The sync switches in settings need the transports even while sync stays stopped.
            expect(installServerSyncProduction).toHaveBeenCalledExactlyOnceWith({ resumeBound: false })
            expect(startup.transports).toHaveBeenCalledOnce()
            expect(startup.stages.mock.calls.flat()).toContain('drive-sync')
            expect(startup.readVault).toHaveBeenCalledOnce()
            expect(startup.database.account).toBe(startup.credential)
        } else {
            expect(initializeNativeSyncBindings).not.toHaveBeenCalled()
            expect(installServerSyncProduction).not.toHaveBeenCalled()
            expect(startup.transports).not.toHaveBeenCalled()
            expect(startup.stages.mock.calls.flat()).not.toContain('drive-sync')
        }
    })

    it('reaches UI when registering the sync transports fails while sync is left off', async () => {
        startup.native = true
        startup.exclusions = ['sync']
        vi.mocked(installServerSyncProduction).mockRejectedValueOnce(new Error('synthetic transport failure'))
        await loadData()
        expect(bootFailure.set).toHaveBeenLastCalledWith(expect.objectContaining({
            stage: 'ui-state', message: startup.stop.message,
        }))
        expect(startup.external).not.toHaveBeenCalled()
        expect(startup.transports).toHaveBeenCalledOnce()
    })

    it.each([true, false])('runs account and sync startup work when enabled (native=%s)', async (native) => {
        startup.native = native
        startup.exclusions = []
        await loadData()
        expect(bootFailure.set).toHaveBeenLastCalledWith(expect.objectContaining({
            stage: 'ui-state', message: startup.stop.message,
        }))
        expect(initializeOfficialAccountBootstrap).toHaveBeenCalledOnce()
        expect(startup.stages.mock.calls.flat()).toContain('account-bootstrap')
        if (native) {
            expect(initializeNativeSyncBindings).toHaveBeenCalledOnce()
            expect(installServerSyncProduction).toHaveBeenCalledOnce()
            expect(startup.reconcile).toHaveBeenCalledOnce()
            expect(startup.external).toHaveBeenCalledOnce()
            expect(startup.transports).not.toHaveBeenCalled()
            expect(startup.stages.mock.calls.flat()).toContain('drive-sync')
        } else {
            expect(initializeNativeSyncBindings).not.toHaveBeenCalled()
            expect(installServerSyncProduction).not.toHaveBeenCalled()
            expect(loadRisuAccountData).toHaveBeenCalledOnce()
            expect(startup.stages.mock.calls.flat()).toContain('service-worker')
        }
    })

    it('attributes a fatal server sync startup failure to the sync stage', async () => {
        startup.native = true
        startup.exclusions = []
        const failure = new Error('synthetic sync startup failure')
        vi.mocked(installServerSyncProduction).mockRejectedValueOnce(failure)
        await loadData()
        expect(bootFailure.set).toHaveBeenLastCalledWith(expect.objectContaining({
            stage: 'drive-sync', message: failure.message,
        }))
        expect(startup.external).not.toHaveBeenCalled()
    })
})

describe('exit sync after a start that left sync off', () => {
    type ExitDependencies = Parameters<typeof createSyncExitCoordinator>[0]
    const selection = (kind: 'server' | 'external', connectionId: string) => ({ kind, connectionId, selectionEpoch: '2', paused: false })
    async function exitTarget(current: ReturnType<typeof selection>, clearedDuringSession = false) {
        startup.native = true
        startup.exclusions = ['sync']
        startup.exitDrain.mockImplementation(() => ({ id: `external:${current.connectionId}:2` }))
        startup.serverDrain.mockImplementation((id: string) => ({ id }))
        vi.mocked(getExternalStorageBridge).mockReturnValue({
            captureExitTarget: async () => ({ revision: '5', libraryEpoch: '1', selection: current }),
        } as never)
        await loadData()
        const dependencies = vi.mocked(createSyncExitCoordinator).mock.calls.at(-1)![0] as ExitDependencies
        if (clearedDuringSession) startup.exclusions = []
        await dependencies.acquireEditFence()
        const target = await dependencies.captureTarget()
        return { target, drain: await dependencies.selectedDrain() }
    }
    beforeEach(() => {
        startup.serverPaused = true
        startup.lwwRunning = false
    })

    it('drains an external target the user started during the session without starting automatic backups', async () => {
        startup.lwwRunning = true
        const { target, drain } = await exitTarget(selection('external', 'connection-1'))
        expect(target.selectionId).toBe('external:connection-1:2')
        expect(drain?.id).toBe('external:connection-1:2')
        expect(startup.external).not.toHaveBeenCalled()
        expect(startup.transports).toHaveBeenCalled()
    })

    it('leaves an external target alone when it was never started', async () => {
        const { target, drain } = await exitTarget(selection('external', 'connection-1'))
        expect(target.selectionId).toBe('none:2')
        expect(drain).toBeNull()
        expect(startup.exitDrain).not.toHaveBeenCalled()
    })

    it('drains a server target the user started during the session', async () => {
        startup.serverPaused = false
        const { target, drain } = await exitTarget(selection('server', 'server-1'))
        expect(target.selectionId).toBe('server:server-1:2')
        expect(drain?.id).toBe('server:server-1:2')
    })

    it('leaves a server target alone when it was never started', async () => {
        const { target, drain } = await exitTarget(selection('server', 'server-1'))
        expect(target.selectionId).toBe('none:2')
        expect(drain).toBeNull()
    })

    it('does not start sync at exit when the exclusion was cleared during the session', async () => {
        const { target, drain } = await exitTarget(selection('external', 'connection-1'), true)
        expect(target.selectionId).toBe('none:2')
        expect(drain).toBeNull()
        expect(startup.external).not.toHaveBeenCalled()
    })
})

describe('renderer recovery notice', () => {
    beforeEach(() => {
        startup.native = true
        startup.exclusions = []
        startup.reachUi = true
        startup.reconcile.mockResolvedValue(undefined)
    })

    it('shows a toast after the terms answer and queued dialogs so neither can replace it', async () => {
        let answerTerms!: (accepted: boolean) => void
        vi.mocked(alertTOS).mockReturnValueOnce(new Promise((resolve) => { answerTerms = resolve }))
        startup.takeRendererRecovery.mockResolvedValueOnce(true)
        await loadData()
        expect(bootFailure.set).toHaveBeenLastCalledWith(null)
        expect(startup.takeRendererRecovery).not.toHaveBeenCalled()
        answerTerms(true)
        await vi.waitFor(() => expect(alertToast).toHaveBeenCalledExactlyOnceWith('Synthetic recovery notice'))
        expect(alertNormal).not.toHaveBeenCalled()
        expect(vi.mocked(waitAlert).mock.invocationCallOrder.at(-1))
            .toBeLessThan(vi.mocked(alertToast).mock.invocationCallOrder[0])
    })

    it('words the notice as a reload with saved data', async () => {
        const [{ languageKorean }, { languageEnglish }] = await Promise.all([import('src/lang/ko'), import('src/lang/en')])
        expect(languageKorean.risuNest.startup.rendererRecovered).toBe('저장된 데이터로 화면을 다시 불러왔습니다.')
        expect(languageEnglish.risuNest.startup.rendererRecovered).toBe('The screen was reloaded with your saved data.')
    })

    it('leaves the answer untaken when the terms are declined', async () => {
        const reload = vi.fn()
        vi.stubGlobal('location', { reload })
        vi.mocked(alertTOS).mockResolvedValueOnce(false)
        await loadData()
        await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce())
        expect(startup.takeRendererRecovery).not.toHaveBeenCalled()
        expect(alertToast).not.toHaveBeenCalled()
    })
})
