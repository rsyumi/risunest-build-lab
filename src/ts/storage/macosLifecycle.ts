import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type {
    LifecycleExitCoordinator,
    LifecycleExitSyncPolicy,
} from './lifecycleCommit'

interface MacosExitBaseDependencies {
    respond(token: string, exit: boolean): Promise<void>
    reportError(error: unknown): void
    /** Saves local data without sync or questions. */
    saveLocally(): Promise<void>
}

interface MacosCoordinatedExitDependencies extends MacosExitBaseDependencies {
    coordinator: LifecycleExitCoordinator
}

interface MacosLegacyExitDependencies extends MacosExitBaseDependencies {
    flush(): Promise<void>
    checkpoint(): Promise<void>
    confirmExitWithoutSaving(): Promise<boolean>
    sync: LifecycleExitSyncPolicy
}

export type MacosExitDependencies =
    | MacosCoordinatedExitDependencies
    | MacosLegacyExitDependencies

export interface MacosExitRequest {
    token: string
    /** Logout, restart or shutdown asked for the quit. */
    sessionEnd: boolean
}

const SESSION_END_SAVE_LIMIT_MILLIS = 2_000

function settleWithin(work: Promise<void>, millis: number): Promise<void> {
    return new Promise((resolve, reject) => {
        const timeout = globalThis.setTimeout(() => {
            reject(new Error(`Session-end save did not settle within ${millis} ms`))
        }, millis)
        work.then(
            () => {
                globalThis.clearTimeout(timeout)
                resolve()
            },
            (error) => {
                globalThis.clearTimeout(timeout)
                reject(error)
            },
        )
    })
}

/** One quit request settles the real save before any acknowledgement reaches Rust. */
export function createMacosExitHandler(
    dependencies: MacosExitDependencies,
    sessionEndLimitMillis = SESSION_END_SAVE_LIMIT_MILLIS,
) {
    let pending = false
    return async ({ token, sessionEnd }: MacosExitRequest): Promise<void> => {
        if (pending) return
        pending = true
        let exit = false
        try {
            if (sessionEnd) {
                // The session ends either way, so a failed or slow save still lets it go.
                try {
                    await settleWithin(dependencies.saveLocally(), sessionEndLimitMillis)
                } catch (error) {
                    dependencies.reportError(error)
                }
                exit = true
                return
            }
            if ('coordinator' in dependencies) {
                exit = await dependencies.coordinator.requestExit() === 'exit'
                return
            }
            try {
                await dependencies.flush()
                await dependencies.checkpoint()
                exit = true
            } catch (error) {
                dependencies.reportError(error)
                exit = await dependencies.confirmExitWithoutSaving()
            }
            if (
                exit &&
                dependencies.sync.isSyncActive() &&
                dependencies.sync.hasPendingSync()
            ) {
                exit = await dependencies.sync.confirmExit()
            }
        } catch (error) {
            exit = false
            dependencies.reportError(error)
        } finally {
            try {
                await dependencies.respond(token, exit)
            } finally {
                pending = false
            }
        }
    }
}

export async function registerMacosLifecycle(
    dependencies:
        | Omit<MacosCoordinatedExitDependencies, 'respond' | 'reportError'>
        | Omit<MacosLegacyExitDependencies, 'respond' | 'reportError'>,
): Promise<() => void> {
    const handler = createMacosExitHandler({
        ...dependencies,
        respond: (token, exit) =>
            invoke('macos_exit_response', { token, exit }),
        reportError: (error) =>
            console.error('macOS quit settlement failed', error),
    })
    const unlisten = await listen<MacosExitRequest>(
        'risu-macos-exit-requested',
        ({ payload }) => {
            void handler(payload).catch((error) =>
                console.error('macOS quit response failed', error),
            )
        },
    )
    try {
        await invoke('macos_lifecycle_ready')
    } catch (error) {
        unlisten()
        throw error
    }
    return unlisten
}
