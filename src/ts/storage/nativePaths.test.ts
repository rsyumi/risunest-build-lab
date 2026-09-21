import { beforeEach, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({
    invoke: vi.fn(async (command: string) => {
        if (command !== 'app_paths_roots') throw new Error(`unexpected ${command}`)
        return { data: '/synthetic/store' }
    }),
}))
vi.mock('@tauri-apps/api/path', () => ({
    join: async (...parts: string[]) => parts.join('/'),
}))

import { invoke } from '@tauri-apps/api/core'
import { iosStagingPath, nativeDataPath, nativeRoots } from './nativePaths'

beforeEach(() => {
    vi.mocked(invoke).mockClear()
})

it('builds every native path from the root Rust reports, asking for it once', async () => {
    expect(await nativeRoots()).toEqual({ data: '/synthetic/store' })
    expect(await nativeDataPath()).toBe('/synthetic/store')
    expect(await nativeDataPath('remotes', 'local.bin')).toBe('/synthetic/store/remotes/local.bin')
    expect(invoke).toHaveBeenCalledTimes(1)
})

it('stages an iOS handoff under the root the native side enforces', async () => {
    const staging = await iosStagingPath()
    const [prefix, folder] = [staging.slice(0, staging.lastIndexOf('/')), staging.split('/').at(-1)]
    expect(prefix).toBe(await nativeDataPath('ios-file-staging'))
    expect(folder).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/)
    expect(await iosStagingPath()).not.toBe(staging)
})
