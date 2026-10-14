import { vi } from 'vitest'

type PluginFs = typeof import('@tauri-apps/plugin-fs')

/** The plugin-fs module with the given file calls replaced; its enums and classes stay real. */
export async function pluginFsModule(fakes: Partial<PluginFs>): Promise<PluginFs> {
    return { ...await vi.importActual<PluginFs>('@tauri-apps/plugin-fs'), ...fakes }
}

/** An opened native file over `bytes` that returns at most `maxRead` bytes per read. */
export function readableFile(bytes: Uint8Array, maxRead = Infinity) {
    let position = 0
    return {
        seek: vi.fn(async (offset: number) => { position = offset }),
        read: vi.fn(async (target: Uint8Array) => {
            const chunk = bytes.subarray(position, position + Math.min(target.length, maxRead))
            target.set(chunk)
            position += chunk.length
            return chunk.length || null
        }),
        close: vi.fn(async () => {}),
    }
}
