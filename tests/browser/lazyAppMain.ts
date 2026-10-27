import { mount } from 'svelte'
import '../../src/styles.css'
import App from '../../src/App.svelte'
import { openRoute, lazyImportBoundary, navigationState, loadedStore, bootFailure, alertStore } from './lazyAppAdapters'
import { appLock } from '../../src/ts/appLock/appLock.svelte'
import { createPinRecord, defaultAppLockRecord } from '../../src/ts/appLock/record'
import { installAppLockInput } from '../../src/ts/appLock/input'
import { createSyncExitCoordinator } from '../../src/ts/storage/syncExitCoordinator'
import { configureSyncExitCoordinator } from '../../src/ts/storage/syncExitProduction'
import { loadV3Plugins } from '../../src/ts/plugins/apiV3/v3.svelte'
import { fixtureState } from './hostAdapters'

installAppLockInput()
const exitCoordinator = createSyncExitCoordinator({
    acquireEditFence: async () => ({ release() {} }),
    flushLocal: async () => { throw new Error('Synthetic local write failure') },
    checkpointLocal: async () => {},
    captureTarget: async () => ({ revision: 1, libraryEpoch: 'synthetic', selectionEpoch: 'synthetic', selectionId: 'synthetic' }),
    selectedDrain: () => null,
})
configureSyncExitCoordinator(exitCoordinator)
const lockDriver = {
    async initialize(nativeUnlock = false) {
        let record = JSON.stringify({ ...defaultAppLockRecord(), enabled: true, timing: 'immediate', nativeUnlock, pin: await createPinRecord('123456') })
        await appLock.initialize({ read: () => record, commit: async value => { record = value } })
    },
    bootFailure(loaded: boolean) {
        loadedStore.set(loaded)
        bootFailure.set({ kind: 'unknown', message: 'Synthetic startup failure', stage: 'synthetic' })
    },
    requestExit() { void exitCoordinator.requestExit() },
    changePin() { appLock.beginChange() },
    loading(value: boolean) { loadedStore.set(!value) },
    loadingAlert() { alertStore.set({ type: 'ask', msg: 'Synthetic startup confirmation' }) },
}
const plugin = {
    async load(script: string) {
        fixtureState.database.plugins = [{ name: 'lock-fixture', script, arguments: {}, realArg: {}, version: '3.0', customLink: [], argMeta: {}, enabled: true }]
        await loadV3Plugins(fixtureState.database.plugins)
    },
    writes: fixtureState.writes,
}
Object.assign(window, { lazyImportBoundary, lazyApp: { open: openRoute, ...navigationState, lock: lockDriver, plugin } })
mount(App, { target: document.querySelector<HTMLElement>('#app')! })
