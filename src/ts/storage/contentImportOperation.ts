import { joinAbortSignals } from '../abortSignals'
import { runSharedNativeFileOperation } from './nativeFileJobManager'
import {
    syntheticNativeFileJobStatus,
    type NativeFileJobOptions,
    type NativeFileJobStatus,
} from './nativeFileJobs'

/** Keep preparation and the revisioned store commit inside one visible operation. */
export function runContentImport<T>(
    name: string,
    options: NativeFileJobOptions,
    operation: (options: NativeFileJobOptions) => Promise<T>,
): Promise<T> {
    if (options.onStatus) return operation(options)
    return runSharedNativeFileOperation(
        'import',
        `content:${name}`,
        async (context) => {
            context.setSource({ name })
            if (!options.signal) return operation({ ...options, signal: context.signal, onStatus: context.onStatus })
            const joined = joinAbortSignals([options.signal, context.signal])
            try {
                return await operation({ ...options, signal: joined.signal, onStatus: context.onStatus })
            } finally {
                joined.dispose()
            }
        },
        { presentation: 'dialog', format: 'content' },
    )
}

export function contentMappingStatus(
    status: NativeFileJobStatus,
): NativeFileJobStatus {
    return syntheticNativeFileJobStatus(status, 'finalizing-staging')
}
