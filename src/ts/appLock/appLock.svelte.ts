import { getDeviceMarkers } from '../storage/deviceMarkers'
import { AppLockController, appLockIsGated, initialAppLockState, type AppLockStorage } from './controller'
import { createAppLockNativeAdapter, normalizeAppLockBackground, type AppLockLifecycle } from './native'
import { APP_LOCK_KEY } from './record'

function markerStorage(): AppLockStorage {
    return {
        read: () => getDeviceMarkers().getItem(APP_LOCK_KEY),
        commit: async (serialized) => {
            const markers = getDeviceMarkers()
            if (!markers.commitItem) throw new Error('App lock requires durable device markers')
            await markers.commitItem(APP_LOCK_KEY, serialized)
        },
    }
}

class AppLockFacade {
    state = $state.raw(initialAppLockState())
    private controller: AppLockController | null = null
    private starting: Promise<void> | null = null
    private backgroundColor = '#21252b'
    private failureUnlisten: (() => void) | null = null
    private failureStarting: Promise<void> | null = null
    private failureGeneration = 0
    get gated(): boolean { return appLockIsGated(this.state) }

    initialize(options: { read?: AppLockStorage['read']; commit?: AppLockStorage['commit']; recovery?: boolean } = {}): Promise<void> {
        const storage = markerStorage()
        if (options.read) storage.read = options.read
        if (options.commit) storage.commit = options.commit
        if (this.controller) {
            const controller = this.controller
            this.starting = (this.starting ?? Promise.resolve()).then(async () => {
                if (options.recovery) controller.attachStorage(storage)
                else await controller.continueNormally(storage)
            })
            return this.starting
        }
        this.failureGeneration++
        this.failureUnlisten?.()
        this.failureUnlisten = null
        this.controller = new AppLockController(storage, createAppLockNativeAdapter())
        const controller = this.controller
        controller.subscribe((state) => { this.state = state })
        this.starting = (this.failureStarting ?? Promise.resolve()).then(async () => {
            controller.setBackgroundColor(this.backgroundColor)
            await controller.initialize(options.recovery)
        })
        return this.starting
    }

    digit(digit: string) { return this.controller?.digit(digit) }
    deleteDigit() { this.controller?.deleteDigit() }
    beginSetup() { this.controller?.beginSetup() }
    beginChange() { this.controller?.beginChange() }
    cancelFlow() { this.controller?.cancelFlow() }
    retrySave() { return this.controller?.retrySave() }
    retryRead() { return this.controller?.retryRead() }
    refreshCapabilities() { return this.controller?.refreshCapabilities() }
    unlockWithDevice(reason: string) { return this.controller?.unlockWithDevice(reason) }
    resetPin(reason: string) { return this.controller?.resetPin(reason) }
    setEnabled(enabled: false) { return this.controller?.setEnabled(enabled) ?? Promise.resolve(false) }
    setTiming(timing: Parameters<AppLockController['setTiming']>[0]) { return this.controller?.setTiming(timing) ?? Promise.resolve(false) }
    setNativeUnlock(enabled: boolean, reason: string) { return this.controller?.setNativeUnlock(enabled, reason) ?? Promise.resolve('saveFailed' as const) }
    setHideRecentsPreview(enabled: boolean) { return this.controller?.setHideRecentsPreview(enabled) ?? Promise.resolve(false) }
    async renderBootFailure(): Promise<void> {
        if (this.controller) return this.controller.renderBootFailure()
        if (this.failureStarting) return this.failureStarting
        const generation = this.failureGeneration
        const native = createAppLockNativeAdapter()
        let presentation = 0
        let latestEpoch = -1
        const present = async (snapshot: AppLockLifecycle) => {
            if (snapshot.epoch < latestEpoch) return
            latestEpoch = snapshot.epoch
            const request = ++presentation
            if (!snapshot.active) return
            for (let attempt = 0; attempt < 4; attempt++) {
                if (generation !== this.failureGeneration || this.controller || request !== presentation) return
                try { if (await native.ready(snapshot.epoch, false)) return } catch { return }
                if (attempt < 3) await new Promise<void>((resolve) => setTimeout(resolve, [32, 128, 512][attempt]))
            }
        }
        this.failureStarting = (async () => {
            if (!this.failureUnlisten) {
                try {
                    const unlisten = await native.listen((snapshot) => { void present(snapshot) })
                    if (generation !== this.failureGeneration || this.controller) unlisten()
                    else this.failureUnlisten = unlisten
                } catch { /* The initial policy snapshot can still release the failure surface. */ }
            }
            if (generation !== this.failureGeneration || this.controller) return
            const snapshot = await native.setPolicy({
                enabled: false, timing: 'launch', gateRequired: false,
                hideRecentsPreview: false, backgroundColor: this.backgroundColor,
            })
            await present(snapshot)
        })().catch(() => undefined).finally(() => { this.failureStarting = null })
        return this.failureStarting
    }
    rendered(reason: string) { return this.controller?.rendered(reason) }
    setBackgroundColor(color: string) {
        const normalized = normalizeAppLockBackground(color)
        if (normalized) {
            this.backgroundColor = normalized
            this.controller?.setBackgroundColor(normalized)
        }
    }
}

export const appLock = new AppLockFacade()
export const initializeAppLock = (options?: Parameters<AppLockFacade['initialize']>[0]) => appLock.initialize(options)
export type { AppLockState, PinFlow, AppLockIssue } from './controller'
export type { AppLockTiming } from './record'
