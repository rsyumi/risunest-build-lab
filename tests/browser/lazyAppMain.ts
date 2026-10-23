import { mount } from 'svelte'
import App from '../../src/App.svelte'
import { openRoute, lazyImportBoundary, navigationState, loadedStore, bootFailure } from './lazyAppAdapters'
import { appLock } from '../../src/ts/appLock/appLock.svelte'
import { createPinRecord, defaultAppLockRecord } from '../../src/ts/appLock/record'
import { installAppLockInput } from '../../src/ts/appLock/input'
import { createSyncExitCoordinator } from '../../src/ts/storage/syncExitCoordinator'
import { configureSyncExitCoordinator } from '../../src/ts/storage/syncExitProduction'

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
    async initialize() {
        let record = JSON.stringify({ ...defaultAppLockRecord(), enabled: true, timing: 'immediate', pin: await createPinRecord('123456') })
        await appLock.initialize({ read: () => record, commit: async value => { record = value } })
    },
    bootFailure(loaded: boolean) {
        loadedStore.set(loaded)
        bootFailure.set({ kind: 'unknown', message: 'Synthetic startup failure', stage: 'synthetic' })
    },
    requestExit() { void exitCoordinator.requestExit() },
    changePin() { appLock.beginChange() },
}
Object.assign(window, { lazyImportBoundary, lazyApp: { open: openRoute, ...navigationState, lock: lockDriver } })
mount(App, { target: document.querySelector<HTMLElement>('#app')! })
