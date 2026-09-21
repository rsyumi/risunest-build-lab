import { invoke } from '@tauri-apps/api/core'
import { join } from '@tauri-apps/api/path'

interface NativeRoots {
    data: string
}

let pending: Promise<NativeRoots> | null = null

/**
 * The native store roots, resolved by Rust. Nothing in the renderer derives a
 * root of its own, so a platform naming decision cannot split the two sides.
 */
export async function nativeRoots(): Promise<NativeRoots> {
    pending ??= invoke<NativeRoots>('app_paths_roots').catch((error) => {
        pending = null
        throw error
    })
    return await pending
}

/** A path inside the native store. */
export async function nativeDataPath(...segments: string[]): Promise<string> {
    const { data } = await nativeRoots()
    return segments.length === 0 ? data : await join(data, ...segments)
}

/** A fresh staging directory for an iOS document-picker handoff. */
export async function iosStagingPath(): Promise<string> {
    return await nativeDataPath('ios-file-staging', crypto.randomUUID())
}
