import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get, type Writable } from 'svelte/store'
import type { PlatformShape } from 'src/ts/platform.testSupport'
import type { createNativeJobHost } from 'src/ts/storage/nativeFileJobs.testSupport'

const h = vi.hoisted(() => ({
    platform: { runtime: 'tauri', os: 'windows' } as PlatformShape,
    host: undefined as ReturnType<typeof createNativeJobHost> | undefined,
    runtime: undefined as unknown,
    refreshRevision: undefined as Writable<unknown> | undefined,
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
vi.mock('src/ts/storage/deviceBackup/selectionDialog', () => ({
    selectPortableBackupRestore: async () => ({library:true, deviceSections:['hypa','local-plugins','local-settings']}),
}))
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
vi.mock('src/ts/storage/persistentDataRuntime.svelte', async () => {
    h.refreshRevision = (await import('svelte/store')).writable<unknown>(null)
    return { getPersistentDataRuntime: () => h.runtime, persistentWorkingSetRefreshRevision: h.refreshRevision }
})
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
import { dismissNativeFileOperationOutcome, nativeFileOperationOutcome } from 'src/ts/storage/nativeFileJobManager'
import { createNativeJobHost as jobHost, restoreRuntime, status } from 'src/ts/storage/nativeFileJobs.testSupport'
import { capturePersistentRoot, createPersistentDataRuntime } from 'src/ts/storage/persistentDataRuntime'
import { encodeRisuSaveLegacy } from 'src/ts/storage/risuSave'
import { createPersistentStoreHarness } from '../../../../tests/support/persistentStoreHarness'
import Onboarding from './Onboarding.svelte'
import { onboardingHold } from './onboardingGate'
import { retryCommittedWorkingSetRefreshWithContinuation } from 'src/ts/storage/committedWorkingSetContinuation'

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

it.each([
    ['block-risu-save', 'refresh'], ['block-risu-save', 'lost-ack'], ['block-risu-save', 'unchanged'],
    ['portable', 'refresh'], ['portable', 'lost-ack'], ['portable', 'bodies'], ['portable', 'bodies-stuck'],
] as const)('recovers the original %s onboarding import after %s without activating twice', async (format, failure) => {
    h.platform = {runtime: 'tauri', os: 'windows'}
    let database = library('Existing', 'existing')
    const storage = await createPersistentStoreHarness(database)
    let refreshFails = false
    const runtime = createPersistentDataRuntime({
        store: storage.store,
        state: {
            captureRoot: () => capturePersistentRoot(database), capturePresets: () => database.botPresets,
            captureSelectedCharacter: () => null, captureCharacter: id => database.characters.find(c => c.chaId === id) ?? null,
            getSelectedCharacterId: () => undefined,
            replaceDatabase: value => { database = structuredClone(value) },
            publishCharacter: vi.fn(), publishConversation: vi.fn(),
        },
        prepareDatabase: async value => { if (refreshFails) throw Error('Synthetic refresh failure'); return value },
        prepareRoot: async value => { if (refreshFails) throw Error('Synthetic refresh failure'); return value },
        onWorkingSetRefreshRequired: revision => h.refreshRevision!.set(revision),
    })
    await runtime.initializeActiveWorkingSet(database)
    h.runtime = runtime
    let activated = false
    let resolved = false
    let selected = false, adopted = false, bodiesRetried = false
    let result = {revision: storage.revision + 1, sourceBytes: 128, sourceFingerprintKind: format === 'portable' ? 'portable-catalog-sha256' as const : 'whole-file-sha256' as const,
        sourceSha256: 'c'.repeat(64), characterCount: 1, presetCount: 0, warningCodes: []}
    h.host = jobHost([], {
        native_backup_source_format: format,
        native_file_job_status: () => {
            const kind = format === 'portable' ? 'restore-portable-backup' : 'restore-block-risu-save'
            if (format === 'portable' && !selected) return {...status('waitingForInput'), kind, phase:'awaiting-backup-selection',
                restorePreview:{libraryIncluded:true, repairRequired:false, deviceSections:['hypa','local-plugins','local-settings']}}
            if (!activated) return {...status('waitingForInput'), kind, phase: 'awaiting-activation'}
            if (!resolved && (failure === 'lost-ack' || failure === 'unchanged')) throw {code: 'activation-outcome-unresolved', message: 'Synthetic pending receipt'}
            if (format === 'portable') {
                const pending = (failure === 'bodies' && !bodiesRetried) || failure === 'bodies-stuck'
                return {...status(!adopted ? 'running' : pending ? 'failed' : 'succeeded', adopted && !pending ? result : undefined), kind,
                    phase: adopted ? 'complete' : 'activating-database', activationRevision:result.revision, activationAuthority:'1',
                    deviceSessionId:'synthetic-device', restoreAdoptionConfirmed:adopted,
                    portableBodyRetry:{pending, available:pending, sourceRequired:false, stagingId:'synthetic-stage', catalogSha256:'c'.repeat(64)},
                }
            }
            return failure === 'unchanged' ? status('cancelled') : status('succeeded', result)
        },
        native_portable_select_sections: () => { selected = true },
        native_device_backup_recovery_complete: () => {},
        pds_open: () => ({revision:result.revision}),
        native_portable_confirm_restore_adoption: () => { adopted = true },
        native_portable_retry_restore_bodies: () => {
            if (failure === 'bodies-stuck') throw {code: 'synthetic-body-failure', message: 'Synthetic body failure'}
            bodiesRetried = true
        },
        native_file_job_finalize: async () => {
            activated = true
            if (failure !== 'unchanged') {
                result = {...result, revision: (await storage.store.replaceFromDatabase(library('Restored', 'restored'))).revision}
            }
            refreshFails = failure === 'refresh' || failure.startsWith('bodies')
        },
    })
    await chooseImport()
    await vi.waitFor(() => expect(get(nativeFileOperationOutcome)?.error?.recoveryRequired).toBe(true))
    await tick()
    expect(h.error).not.toHaveBeenCalled()
    expect(get(onboardingHold)).toBe(true)
    const labels = () => [...target.querySelectorAll('button')].map(b => b.textContent?.trim())
    expect(labels()).not.toContain(language.risuNest.importDialog.close)
    // The screen still waits for its refresh, so the reader cannot move on yet.
    expect(labels()).not.toContain(language.risuNest.onboarding.import.next)
    if (failure === 'lost-ack' || failure === 'unchanged') {
        await runtime.retryCommittedWorkingSetRefresh().catch(() => {})
        expect(() => runtime.assertPersistentMutationAllowed()).toThrow()
        expect(get(onboardingHold)).toBe(true)
    }
    resolved = true
    refreshFails = false
    await retryCommittedWorkingSetRefreshWithContinuation(runtime)
    await tick()
    if (failure.startsWith('bodies')) {
        expect(get(onboardingHold)).toBe(true)
        expect(get(nativeFileOperationOutcome)?.status?.portableBodyRetry?.pending).toBe(true)
        expect(target.textContent).toContain(language.risuNest.importDialog.resultAssetsIncomplete)
        expect(labels()).toEqual(expect.arrayContaining([language.retry, language.risuNest.onboarding.import.next]))
        const retry = [...target.querySelectorAll('button')].find(b => b.textContent?.trim() === language.retry)
        retry!.click()
        if (failure === 'bodies-stuck') {
            // The failed retry never read a job status, so it cannot offer another retry.
            await vi.waitFor(() => {
                const outcome = get(nativeFileOperationOutcome)
                expect([outcome?.error?.code, outcome?.status]).toEqual(['portable-bodies-incomplete', undefined])
            })
            await tick()
            expect(get(onboardingHold)).toBe(true)
            expect(target.textContent).toContain(language.risuNest.importDialog.resultAssetsIncomplete)
            expect(labels()).not.toContain(language.retry)
            // The data is restored, so the missing assets do not keep the reader in onboarding.
            const next = [...target.querySelectorAll('button')].find(b => b.textContent?.trim() === language.risuNest.onboarding.import.next)
            next!.click()
        }
        await vi.waitFor(() => expect(get(onboardingHold)).toBe(false))
        await tick()
    }
    if (failure === 'unchanged') {
        expect(get(onboardingHold)).toBe(true)
        expect(get(nativeFileOperationOutcome)?.state).toBe('cancelled')
        expect(get(nativeFileOperationOutcome)?.error?.recoveryRequired).toBe(false)
        expect(database.username).toBe('Existing')
        expect(h.state.db.didFirstSetup).not.toBe(true)
    } else {
        expect(get(onboardingHold)).toBe(false)
        expect(target.textContent).toContain(language.risuNest.onboarding.done.title)
        expect(database.username).toBe('Restored')
        expect(get(nativeFileOperationOutcome)).toBeNull()
        expect(() => runtime.assertPersistentMutationAllowed()).not.toThrow()
    }
    expect(h.host.commands().filter(c => c === 'native_file_job_start')).toHaveLength(1)
    expect(h.host.commands().filter(c => c === 'native_file_job_finalize')).toHaveLength(1)
})

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
