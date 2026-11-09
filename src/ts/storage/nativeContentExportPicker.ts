import {
    reserveAndroidSafDestination,
    withAndroidSafDestination,
    type AndroidSafDestinationReservation,
    type AndroidSafExportKind,
} from './androidSafBridge'

export interface NativeContentExportRuntime {
    readonly revision: number
    flushPendingData(reason: string): Promise<void>
}

export interface NativeContentExportPickerDependencies {
  isDesktop(): boolean;
  isAndroid(): boolean;
  isIOS?(): boolean;
  runtime(): NativeContentExportRuntime;
  reserveAndroidDestination?: typeof reserveAndroidSafDestination;
}

export type NativeContentExportDestination =
  | { type: "desktopPath"; path: string }
  | { type: "androidSaf"; suggestedName: string; reservation?: AndroidSafDestinationReservation }
  | { type: "iosFiles"; suggestedName: string };

export type NativeContentExportPickerFlow =
    /** Neither desktop nor Android: callers fall back to the compatibility path. */
    | { kind: 'unsupported' }
    /** The desktop save dialog was dismissed without choosing a destination. */
    | { kind: 'cancelled' }
    | {
        kind: 'ready'
        destination: NativeContentExportDestination
        expectedRevision: number
    }

/**
 * Shared platform-check/picker/flush/abort/revision flow for the native
 * content-export routes (character card, CharX, RISUM). The Android save
 * picker opens before the flush, so writes made while it is open precede the
 * captured revision; run the export inside `withNativeContentExportDestination`.
 */
export async function prepareNativeContentExportFromPicker(
    input: {
        suggestedName: string
        flushReason: string
        androidSourceKind?: AndroidSafExportKind
        chooseDestination(): Promise<string | null>
    },
    options: { signal?: AbortSignal },
    dependencies: NativeContentExportPickerDependencies,
): Promise<NativeContentExportPickerFlow> {
    if (
      !dependencies.isDesktop() &&
      !dependencies.isAndroid() &&
      !dependencies.isIOS?.()
    )
      return { kind: "unsupported" };
    const destination = dependencies.isDesktop()
        ? await input.chooseDestination()
        : undefined
    if (dependencies.isDesktop() && !destination) return { kind: 'cancelled' }
    const ios = !destination && dependencies.isIOS?.() === true
    const reservation = destination || ios
        ? undefined
        : await (dependencies.reserveAndroidDestination ?? reserveAndroidSafDestination)(
            input.suggestedName, input.androidSourceKind ?? 'risuSave', options.signal)
            .catch((error: unknown) => {
                // Backing out of the save picker cancels like a desktop picker; a cancelled job still rejects.
                if (!options.signal?.aborted && error instanceof DOMException && error.name === 'AbortError') return null
                throw error
            })
    if (reservation === null) return { kind: 'cancelled' }
    try {
        const runtime = dependencies.runtime()
        await runtime.flushPendingData(input.flushReason)
        if (options.signal?.aborted) {
            throw new DOMException('Native file job was cancelled', 'AbortError')
        }
        return {
          kind: "ready",
          destination: destination
            ? { type: "desktopPath", path: destination }
            : ios
              ? { type: "iosFiles", suggestedName: input.suggestedName }
              : { type: "androidSaf", suggestedName: input.suggestedName, reservation },
          expectedRevision: runtime.revision,
        };
    } catch (error) {
        return withAndroidSafDestination(reservation, () => Promise.reject(error))
    }
}

/** Runs the export and then releases an Android destination the picker reserved. */
export function withNativeContentExportDestination<T>(
    destination: NativeContentExportDestination,
    run: () => Promise<T>,
): Promise<T> {
    return withAndroidSafDestination(destination.type === 'androidSaf' ? destination.reservation : undefined, run)
}
