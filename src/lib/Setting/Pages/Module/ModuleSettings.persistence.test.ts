import { describe, expect, it, vi } from 'vitest'
import { readFileSync } from 'node:fs'
const moduleSettingsSource = readFileSync('src/lib/Setting/Pages/Module/ModuleSettings.svelte', 'utf8')
import { commitModuleCharacterConversion } from './moduleCharacterConversion'

describe('module character conversion persistence', () => {
    it('awaits the named detached addition before reporting success', () => {
        expect(moduleSettingsSource).toContain('await commitModuleCharacterConversion(char')
        expect(moduleSettingsSource).toContain('commit: commitDetachedCharacter')
        expect(moduleSettingsSource).not.toContain('DBState.db.characters.push(char)')
    })

    it('reports success only after durable addition', async () => {
        let resolve!: () => void
        const pending = new Promise<void>(accept => { resolve = accept })
        const onSuccess = vi.fn()
        const onError = vi.fn()
        const operation = commitModuleCharacterConversion({ chaId: 'converted' } as any, {
            commit: vi.fn(() => pending), onSuccess, onError,
        })
        await Promise.resolve()
        expect(onSuccess).not.toHaveBeenCalled()
        expect(onError).not.toHaveBeenCalled()
        resolve()
        await operation
        expect(onSuccess).toHaveBeenCalledOnce()
        expect(onError).not.toHaveBeenCalled()
    })

    it('settles a rejection with one error alert', async () => {
        const failure = new Error('durable commit failed')
        const onSuccess = vi.fn()
        const onError = vi.fn()
        const commit = vi.fn(async () => { throw failure })

        await expect(commitModuleCharacterConversion({ chaId: 'converted' } as any, {
            commit,
            onSuccess,
            onError,
        })).resolves.toBeUndefined()

        expect(commit).toHaveBeenCalledOnce()
        expect(onError).toHaveBeenCalledOnce()
        expect(onError).toHaveBeenCalledWith(failure)
        expect(onSuccess).not.toHaveBeenCalled()
    })
})
