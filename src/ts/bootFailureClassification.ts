import type { BootFailure } from './stores.svelte'

export type BootStage =
    | 'startup'
    | 'native-setup'
    | 'app-data-directories'
    | 'browser-storage'
    | 'persistent-storage'
    | 'device-settings'
    | 'native-log'
    | 'native-file-jobs'
    | 'persistent-database'
    | 'plugin-compatibility-data'
    | 'account-bootstrap'
    | 'service-worker'
    | 'format-update'
    | 'plugins'
    | 'account-data'
    | 'drive-sync'
    | 'ui-state'

/**
 * Boot stages whose failures mean the local persistent store could not be
 * opened or bootstrapped. These stages initialize the persistent working set.
 */
const persistentStoreOpenStages = new Set<BootStage>([
    'persistent-storage',
    'device-settings',
    'persistent-database',
])

function describeBootFailureError(error: unknown): string {
    if (error instanceof Error) return error.message
    if (typeof error === 'string') return error
    if (error && typeof error === 'object') {
        const message = (error as { message?: unknown }).message
        if (typeof message === 'string') return message
    }
    return String(error)
}

/**
 * Classifies a startup failure so the recovery panel can explain what the user
 * has to do. Pure so it can be tested without booting the application.
 */
export function classifyBootFailure(error: unknown, stage?: BootStage): BootFailure {
    const message = describeBootFailureError(error)
    if (message.includes('unsupported persistent schema version')) {
        return { kind: 'schema-unsupported', message, stage }
    }
    if (stage !== undefined && persistentStoreOpenStages.has(stage)) {
        return { kind: 'store-open', message, stage }
    }
    return { kind: 'unknown', message, stage }
}

