import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get } from 'svelte/store'
import type { ServerSyncSnapshot } from 'src/ts/storage/sync/serverSyncController'

const state = vi.hoisted(() => ({ snapshot: undefined as ServerSyncSnapshot | undefined,
    listener: undefined as ((snapshot: ServerSyncSnapshot) => void) | undefined,
    synchronize: vi.fn(async () => {}), bind: vi.fn(async () => {}), reregister: vi.fn(async () => {}),
    db: { language: 'en', didFirstSetup: false },
}))
vi.mock('src/ts/platform', () => ({ isTauri: true, isTauriAndroid: false, isTauriIOS: false }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish, changeLanguage: vi.fn() }))
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: state.db } }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(), alertError: vi.fn(), alertNormal: vi.fn() }))
vi.mock('src/ts/characterCards', () => ({ hubURL: 'https://example.invalid/' }))
vi.mock('src/ts/globalApi.svelte', () => ({ getVersionString: () => 'fixture' }))
vi.mock('src/ts/gui/colorscheme', () => ({ updateTextThemeAndCSS: vi.fn() }))
vi.mock('src/ts/process/templates/templates', () => ({ prebuiltPresets: [] }))
vi.mock('src/ts/storage/database.svelte', () => ({ setPreset: vi.fn() }))
vi.mock('src/ts/storage/nativeFileJobManager', async () => {
    const { writable } = await import('svelte/store')
    return { nativeFileJobHost: writable('dialog'), nativeFileOperation: writable(null), nativeFileOperationOutcome: writable(null),
        cancelActiveNativeFileOperation: vi.fn(), dismissNativeFileOperationOutcome: vi.fn() }
})
vi.mock('src/ts/storage/portableBackupFileRouteProduction.svelte', () => ({ restoreBackupFromSystemPicker: vi.fn() }))
vi.mock('src/ts/storage/risuSaveFileRouteProduction.svelte', () => ({ importRisuSaveFromSystemPicker: vi.fn() }))
vi.mock('src/ts/storage/sync/external/bridge', () => ({ getExternalStorageBridge: () => ({}) }))
vi.mock('src/ts/storage/sync/external/production', () => ({ refreshExternalStorageProductionState: vi.fn(), requestExternalStorageNow: vi.fn(), requestExternalStorageResolveConflict: vi.fn(), requestExternalStorageRestore: vi.fn() }))
vi.mock('src/ts/storage/sync/nativeOfficialAccountFlow', () => ({ getNativeOfficialAccountFlow: vi.fn() }))
vi.mock('src/lib/Setting/ExternalStorage/ConnectionForm.svelte', () => ({ default: () => {} }))
vi.mock('./onboardingWeave', () => ({ observeOnboardingWeave: () => () => {} }))
vi.mock('src/ts/storage/sync/serverSyncProduction', () => ({ getServerSyncController: () => ({
    snapshot: () => state.snapshot, subscribe: (listener: typeof state.listener) => { state.listener = listener; return () => { state.listener = undefined } },
    synchronize: state.synchronize, bind: state.bind, reregister: state.reregister,
}) }))

import Onboarding from './Onboarding.svelte'
import { onboardingHold } from './onboardingGate'
import { languageEnglish } from 'src/lang/en'
const text = languageEnglish.risuNest.onboarding
const sync = languageEnglish.risuNest.serverSync
let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement
const head = { libraryId: 'library', epoch: 'epoch', seq: '1', headId: 'head', minRetainedSeq: '0', sections: {
    hypa: { stateId: 'h', changedSeq: '0', gcFloor: '0' }, library: { stateId: 'l', changedSeq: '0', gcFloor: '0' }, 'local-plugins': { stateId: 'p', changedSeq: '0', gcFloor: '0' },
} }
function paused(): ServerSyncSnapshot {
    return { running: false, paused: true, error: 'cancelled', attemptId: 1, initialSyncComplete: false,
        status: { configured: true, localRevision: 3, endpoint: 'https://example.invalid/', libraryId: 'library', deviceId: 'device', head,
            dirtyRecords: 0, fullScan: false, registrationRequired: false, operationPending: false, pendingDeviceSections: false, reconciling: false } }
}
const button = (label: string) => [...target.querySelectorAll('button')].find((node) => node.textContent?.trim() === label)!
beforeEach(() => {
    vi.clearAllMocks(); state.db.didFirstSetup = false; state.snapshot = paused()
    target = document.createElement('div'); document.body.append(target)
})
afterEach(async () => { if (component) await unmount(component); component = undefined; target.remove() })

describe('mounted server sync onboarding', () => {
    it('retains the paused screen through repeated publications and resumes to verified completion', async () => {
        component = mount(Onboarding, { target }); await tick(); await tick()
        state.listener?.(paused()); await tick()
        expect(target.textContent).toContain(text.hub.pausedSummary)
        expect(button(text.hub.resume)).toBeDefined()
        expect(button(text.done.start)).toBeDefined()
        expect(state.db.didFirstSetup).toBe(false)
        button(text.hub.resume).click(); await tick()
        expect(state.synchronize).toHaveBeenCalledOnce()
        state.listener?.({ ...paused(), paused: false, error: '', initialSyncComplete: true,
            attemptIdentity: { endpoint: 'https://example.invalid/', libraryId: 'library', deviceId: 'device' },
            result: { endpoint: 'https://example.invalid/', phase: 'idle', localRevision: 3, head, conflictCount: 0, conflicts: [], appliedRecords: 0, proposedRecords: 0 } })
        await tick(); await tick()
        expect(target.textContent).not.toContain(text.hub.pausedSummary)
        expect(button(text.done.start)).toBeDefined()
    })
    it('finishes explicitly while paused and releases the onboarding hold', async () => {
        component = mount(Onboarding, { target }); await tick(); await tick()
        expect(get(onboardingHold)).toBe(true)
        button(text.done.start).click(); await tick()
        expect(state.db.didFirstSetup).toBe(true)
        expect(get(onboardingHold)).toBe(false)
        expect(state.synchronize).not.toHaveBeenCalled()
    })
    it('offers re-registration for a refused configured device', async () => {
        state.snapshot = { ...paused(), paused: false, error: 'unauthorized', errorRetryable: false }
        component = mount(Onboarding, { target }); await tick(); await tick()
        expect(target.textContent).toContain(sync.registrationRefusedHelp)
        button(sync.otherCode).click(); await tick()
        button(sync.manualEntry).click(); await tick()
        const inputs = [...target.querySelectorAll<HTMLInputElement>('form.fields input')]
        for (const [i, value] of ['https://example.invalid/', 'library', 'new-device', 'b'.repeat(64)].entries()) {
            inputs[i].value = value; inputs[i].dispatchEvent(new Event('input', { bubbles: true }))
        }
        target.querySelector('form.fields')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); await tick()
        target.querySelector('form.review-form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); await tick()
        expect(state.reregister).toHaveBeenCalledWith(expect.objectContaining({ deviceId: 'new-device' }), 'full')
        expect(state.bind).not.toHaveBeenCalled()
    })
})
