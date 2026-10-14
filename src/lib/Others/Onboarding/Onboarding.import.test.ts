import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import type { PlatformShape } from 'src/ts/platform.testSupport'
import type { createNativeJobHost } from 'src/ts/storage/nativeFileJobs.testSupport'

const h = vi.hoisted(() => ({
    platform: { runtime: 'tauri', os: 'windows' } as PlatformShape,
    host: undefined as ReturnType<typeof createNativeJobHost> | undefined,
    runtime: undefined as unknown,
    file: undefined as File | undefined,
    checkbox: vi.fn(),
    confirm: vi.fn(),
    error: vi.fn(),
    state: { db: {} as Record<string, unknown> },
}))
vi.mock('@tauri-apps/api/core', async (original) => ({
    ...await original<typeof import('@tauri-apps/api/core')>(),
    invoke: (command: string, args?: Record<string, unknown>) => h.host!.dependencies.invoke(command, args),
}))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: async () => 'C:\\synthetic\\library.risudat', save: vi.fn() }))
vi.mock('@tauri-apps/plugin-fs', () => ({ stat: async () => ({ size: 128 }) }))
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'windows' }))
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule(() => h.platform))
vi.mock('src/ts/alert', () => ({ alertConfirm: h.confirm, alertCheckboxConfirm: h.checkbox, alertActionConfirm: vi.fn(), alertError: h.error, alertNormal: vi.fn(), openRisuAccountLogin: vi.fn() }))
// The browser file picker hands over the chosen RisuSave.
vi.mock('src/ts/util', () => ({ selectFileByDom: async () => h.file ? [h.file] : null }))
// The library holds characters, and no sync target is connected.
vi.mock('src/ts/storage/libraryCharacters', () => ({ hasLibraryCharacters: async () => true }))
vi.mock('src/ts/storage/sync/bindingRegistry', () => ({
    bindSyncTarget: vi.fn(),
    prepareBoundLibraryReplacement: async () => ({ bound: false, state: { targetAuthority: '1' }, fence: async () => {}, assertAuthority: async () => {}, resume: async () => {} }),
}))
vi.mock('src/ts/storage/sync/bindingNative', () => ({ createNativeSyncBindingBridge: () => ({ state: async () => ({ target: { kind: 'none' } }) }) }))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({ getPersistentDataRuntime: () => h.runtime }))
vi.mock('src/ts/plugins/apiV3/v3.svelte', () => ({ fencePluginExecutionForAuthorityReplacement: async () => {}, invalidatePluginCachesAfterAuthorityReplacement: async () => {}, restartPluginsAfterAuthorityReplacement: async () => {} }))
vi.mock('src/ts/plugins/plugins.svelte', () => ({ loadPlugins: async () => {}, loadPluginsAfterAuthoritativeRestore: async () => {} }))
vi.mock('src/ts/storage/deviceStateRestore', () => ({ flushDeviceStateBeforeRestore: async () => {}, refreshDeviceStateAfterRestore: async () => {} }))
vi.mock('src/ts/storage/sync/serverSyncProduction', () => ({
    connectServerSync: vi.fn(), dismissServerSyncConnectionFailure: vi.fn(), holdServerSyncAfterRestore: vi.fn(), resumeServerSyncAfterBackup: vi.fn(),
    getServerSyncController: () => ({ snapshot: () => ({ status: { configured: false }, paused: false }), subscribe: () => () => {}, ensureStatus: vi.fn(), assertFileOperationAvailable() {} }),
}))
vi.mock('src/ts/storage/sync/serverSyncQr', () => ({ canScanServerRegistration: false }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountOperations', () => ({ restoreNativeOfficialAccountBackup: vi.fn() }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountFlow', () => ({ NativeAccountLoginError: class extends Error {}, getNativeOfficialAccountFlow: vi.fn() }))
vi.mock('src/ts/storage/sync/external/bridge', () => ({ getExternalStorageBridge: () => ({}) }))
vi.mock('src/ts/storage/sync/external/production', () => ({ refreshExternalStorageProductionState: vi.fn(), requestExternalStorageRestore: vi.fn() }))
vi.mock('src/ts/storage/officialAccountMessage', () => ({ isExpectedHubMessage: vi.fn(), resolveExpectedOfficialAccountMessageUrl: vi.fn() }))
vi.mock('src/ts/characterCards', () => ({ hubURL: 'https://synthetic.invalid' }))
vi.mock('src/ts/globalApi.svelte', () => ({ getVersionString: () => 'synthetic', downloadFile: vi.fn() }))
vi.mock('src/ts/gui/colorscheme', () => ({ updateTextThemeAndCSS: vi.fn() }))
vi.mock('src/ts/process/templates/templates', () => ({ prebuiltPresets: {} }))
vi.mock('src/ts/storage/database.svelte', () => ({ setPreset: vi.fn() }))
vi.mock('src/ts/stores.svelte', () => ({ DBState: h.state }))
vi.mock('src/lib/Setting/Pages/ServerSyncSettings.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/Setting/ExternalStorage/ConnectionForm.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/Others/Onboarding/onboardingWeave', () => ({ observeOnboardingWeave: () => () => {} }))
import { language } from 'src/lang'
import type { Database } from 'src/ts/storage/database.svelte'
import { dismissNativeFileOperationOutcome } from 'src/ts/storage/nativeFileJobManager'
import { createNativeJobHost as jobHost, restoreRuntime, status } from 'src/ts/storage/nativeFileJobs.testSupport'
import { capturePersistentRoot, createPersistentDataRuntime } from 'src/ts/storage/persistentDataRuntime'
import { encodeRisuSaveLegacy } from 'src/ts/storage/risuSave'
import { createPersistentStoreHarness } from '../../../../tests/support/persistentStoreHarness'
import Onboarding from './Onboarding.svelte'

const library = (username: string, characterId: string) => ({
    username,
    botPresets: [],
    characters: [{ type: 'character', chaId: characterId, name: characterId, chats: [] }],
}) as unknown as Database

let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement
beforeEach(() => {
    vi.clearAllMocks()
    // A question, if one appeared, is declined, so the import ends either way.
    h.checkbox.mockResolvedValue({ confirmed: false, checked: false })
    h.confirm.mockResolvedValue(false)
    h.state.db = { language: 'en', characters: [] }
    target = document.createElement('div')
    document.body.append(target)
})
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    target.remove()
    // The finished operation stays on screen until it is closed.
    await dismissNativeFileOperationOutcome()
})

async function chooseImport() {
    const choose = (label: string) => {
        const button = [...target.querySelectorAll('button')].find((candidate) =>
            candidate.querySelector('b')?.textContent === label || candidate.textContent?.trim() === label)
        expect(button).toBeDefined()
        button!.click()
    }
    component = mount(Onboarding, { target })
    await tick()
    choose(language.risuNest.onboarding.home.importTitle)
    await tick()
    choose(language.risuNest.onboarding.import.choose)
}

const asked = () => h.checkbox.mock.calls.length + h.confirm.mock.calls.length > 0

it('restores a backup picked during onboarding over a library with characters without asking', async () => {
    h.platform = { runtime: 'tauri', os: 'windows' }
    const result = { revision: 18, sourceBytes: 128, sourceFingerprintKind: 'whole-file-sha256' as const, sourceSha256: 'a'.repeat(64), characterCount: 1, presetCount: 0, warningCodes: [] }
    h.host = jobHost([{ ...status('waitingForInput'), phase: 'awaiting-activation' }, status('succeeded', result)], {
        native_backup_source_format: 'block-risu-save',
    })
    const acquire = vi.fn()
    h.runtime = restoreRuntime(17, { acquire })
    await chooseImport()

    await vi.waitFor(() => expect(asked() || h.host!.commands().includes('native_file_job_forget')).toBe(true), { timeout: 3000 })
    expect(h.checkbox).not.toHaveBeenCalled()
    expect(h.confirm).not.toHaveBeenCalled()
    expect(h.error).not.toHaveBeenCalled()
    expect(acquire).toHaveBeenCalledOnce()
    expect(h.host.commands()).toContain('native_file_job_finalize')
    expect(h.host.commands()).not.toContain('native_file_job_cancel')
})

it('imports a RisuSave picked during onboarding on the web over a library with characters without asking', async () => {
    h.platform = { runtime: 'web', os: 'windows' }
    h.file = new File([encodeRisuSaveLegacy(library('Imported user', 'imported'))], 'library.risudat')
    let database = library('Existing user', 'existing')
    const storage = await createPersistentStoreHarness(structuredClone(database))
    const runtime = createPersistentDataRuntime({
        store: storage.store,
        state: {
            captureRoot: () => capturePersistentRoot(database),
            capturePresets: () => database.botPresets,
            captureSelectedCharacter: () => null,
            captureCharacter: (id) => database.characters.find((character) => character.chaId === id) ?? null,
            getSelectedCharacterId: () => undefined,
            replaceDatabase: (replacement) => { database = structuredClone(replacement) },
            publishCharacter: vi.fn(),
            publishConversation: vi.fn(),
        },
        prepareDatabase: async (value) => value,
    })
    await runtime.initializeActiveWorkingSet(database)
    h.runtime = runtime
    await chooseImport()

    const imported = async () => (await storage.store.readRoot()).value.username === 'Imported user'
    await vi.waitFor(async () => expect(asked() || h.error.mock.calls.length > 0 || await imported()).toBe(true), { timeout: 3000 })
    expect(h.checkbox).not.toHaveBeenCalled()
    expect(h.confirm).not.toHaveBeenCalled()
    expect(h.error).not.toHaveBeenCalled()
    const stored = await storage.store.materializeDatabase((await storage.store.readRoot()).revision)
    expect(stored.username).toBe('Imported user')
    expect(stored.characters.map((character) => character.chaId)).toEqual(['imported'])
})
