export interface LwwSchedulerDependencies {
    available(): boolean
    publish(): Promise<void>
    receive(): Promise<number>
    maintain?(): Promise<void>
    failed(error: unknown): void
    now?(): number
    setTimer?(callback: () => void, delay: number): unknown
    clearTimer?(timer: unknown): void
}

export function createLwwScheduler(dependencies: LwwSchedulerDependencies) {
    const now = dependencies.now ?? (() => performance.now())
    const setTimer = dependencies.setTimer ?? ((callback, delay) => setTimeout(callback, delay))
    const clearTimer = dependencies.clearTimer ?? (timer => clearTimeout(timer as ReturnType<typeof setTimeout>))
    let enabled = false
    let firstDirty: number | undefined
    let publishTimer: unknown
    let receiveTimer: unknown
    let maintenanceTimer: unknown
    let maintenance: Promise<void> | undefined
    let serial = Promise.resolve()
    let emptyListings = 0
    const queue = <T>(operation: () => Promise<T>): Promise<T> => {
        const task = serial.then(operation)
        serial = task.then(() => {}, error => { dependencies.failed(error) })
        return task
    }
    const clearPublish = () => { if (publishTimer !== undefined) clearTimer(publishTimer); publishTimer = undefined }
    const clearReceive = () => { if (receiveTimer !== undefined) clearTimer(receiveTimer); receiveTimer = undefined }
    const clearMaintenance = () => { if (maintenanceTimer !== undefined) clearTimer(maintenanceTimer); maintenanceTimer = undefined }
    const scheduleMaintenance = () => {
        if (!enabled || !dependencies.maintain || maintenanceTimer !== undefined || maintenance) return
        maintenanceTimer = setTimer(() => {
            maintenanceTimer = undefined
            if (enabled && dependencies.available() && firstDirty === undefined && !maintenance) {
                maintenance = dependencies.maintain!().catch(dependencies.failed).finally(() => { maintenance = undefined; scheduleMaintenance() })
            } else scheduleMaintenance()
        }, 60_000)
    }
    const scheduleReceive = () => {
        clearReceive()
        if (!enabled) return
        receiveTimer = setTimer(() => {
            receiveTimer = undefined
            void receiveNow().catch(() => {})
            scheduleMaintenance()
        }, emptyListings >= 3 ? 60_000 : 20_000)
    }
    const resetListing = () => {
        const backedOff = emptyListings >= 3
        emptyListings = 0
        if (backedOff && receiveTimer !== undefined) scheduleReceive()
    }
    const receiveNow = async (force = false) => {
        clearReceive()
        if (!force && (!enabled || !dependencies.available())) { scheduleReceive(); return }
        try {
            const count = await queue(dependencies.receive)
            emptyListings = count ? 0 : emptyListings + 1
        } finally { scheduleReceive() }
    }
    const publishNow = async (force = false) => {
        clearPublish()
        if (!force && (!enabled || !dependencies.available())) return
        const captured = firstDirty
        try {
            await queue(dependencies.publish)
            if (firstDirty === captured) firstDirty = undefined
        } catch (error) {
            if (enabled && firstDirty !== undefined) {
                const retryAt = Number((error as { retryAtMs?: string | number })?.retryAtMs)
                publishTimer = setTimer(() => { publishTimer = undefined; void publishNow().catch(() => {}) }, Math.max(15_000, Number.isFinite(retryAt) ? retryAt - Date.now() : 15_000))
            }
            throw error
        }
    }
    return {
        dirty(generationComplete = false) {
            if (firstDirty === undefined) firstDirty = now()
            clearPublish()
            if (!enabled) return
            resetListing()
            if (generationComplete) { void publishNow().catch(() => {}); return }
            const delay = Math.max(0, Math.min(15_000, firstDirty + 60_000 - now()))
            publishTimer = setTimer(() => { publishTimer = undefined; void publishNow().catch(() => {}) }, delay)
        },
        /** A previous session can leave an outbox or a sealed publication, so starting publishes before it lists. */
        start(publish = true) {
            if (enabled) return
            enabled = true
            emptyListings = 0
            scheduleMaintenance()
            if (publish) {
                if (firstDirty === undefined) firstDirty = now()
                void publishNow().catch(() => {})
            } else if (firstDirty !== undefined) this.dirty()
            void receiveNow().catch(() => {})
        },
        stop() { enabled = false; clearPublish(); clearReceive(); clearMaintenance() },
        resumeForeground(publish = true) {
            if (!enabled) { this.start(publish); return serial }
            emptyListings = 0
            if (publish) {
                if (firstDirty === undefined) firstDirty = now()
                void publishNow().catch(() => {})
            }
            return receiveNow()
        },
        conversationOpened() { emptyListings = 0; return receiveNow() },
        publishNow,
        receiveNow,
        settled: () => serial,
    }
}
