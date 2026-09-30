import { runContentImport } from './contentImportOperation'
import { language } from 'src/lang'
import { NativeFileJobError } from './nativeFileJobs'
import { importDesktopNativeCharacterPath, type NativeCharacterFileRoutePathDependencies, type NativeCharacterFileRouteResult } from './nativeCharacterFileRoute'

export async function importDesktopNativeCharacterPathInOperation<T>(
    path: string,
    dependencies: NativeCharacterFileRoutePathDependencies<T>,
): Promise<NativeCharacterFileRouteResult<T> | { kind: 'failed' }> {
    let started = false
    try {
        const result = await runContentImport(path.split(/[\\/]/).at(-1) || path, {}, async (options) => {
            started = true
            const outcome = await importDesktopNativeCharacterPath(path, {
                ...dependencies,
                nativeImport: input => dependencies.nativeImport(input, options),
            })
            if (outcome.kind === 'destination-required') {
                throw new NativeFileJobError('destination-required', language.risuNest.importDialog.reasonPlainJpeg)
            }
            return outcome.kind === 'imported' ? outcome : null
        })
        return result ?? { kind: 'declined' }
    } catch (error) {
        if (!started) throw error
        // The shared operation already presents the failure and owns retry state.
        return { kind: 'failed' }
    }
}
