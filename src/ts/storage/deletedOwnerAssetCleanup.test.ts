import { afterEach, describe, expect, it, vi } from 'vitest'

const platform = vi.hoisted(() => ({ runtime: 'tauri' as 'tauri' | 'web' }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule(() => ({ runtime: platform.runtime, os: 'windows' })))
const core = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', async (importOriginal) => ({ ...await importOriginal<typeof import('@tauri-apps/api/core')>(), invoke: core.invoke }))

import { collectCharacterConversationAssetKeys, collectOwnedAssetKeys, scheduleDeletedOwnerAssetCleanup } from './deletedOwnerAssetCleanup'
import { createAssetAliasCleanupTarget } from './deletedOwnerAssetCleanup.testSupport'

afterEach(() => {
    platform.runtime = 'tauri'
    vi.clearAllMocks()
    vi.restoreAllMocks()
})

describe('collectOwnedAssetKeys', () => {
    it('collects every owned key a character, module or persona names, wherever it is stored', () => {
        const character = {
            image: 'assets/portrait.png',
            emotionImages: [['joy', 'assets/joy.png']],
            additionalAssets: [['extra', 'assets/extra.webp', 'webp']],
            desc: '<div><img src="assets/inline.png" alt=""></div> {{raw::assets/raw.png}}',
            firstMessage: 'assets\\backslash.png',
            ccAssets: [{ type: 'icon', uri: 'ccdefault:', name: 'main', ext: 'png' }],
            note: 'sync-conflict-backups/backup.risudat assets/',
            // Within 4096 UTF-16 units but over the 4096 bytes the native cleanup accepts.
            longName: `assets/${'가'.repeat(1400)}.png`,
        }
        const module = { assets: [['map', 'assets/module.png', 'png']], lorebook: [{ content: 'see assets/lore.png' }] }
        const persona = { icon: 'assets/icon.png', embeddedModule: { assets: [['badge', 'assets/embedded.png', 'png']] } }

        expect(new Set(collectOwnedAssetKeys(character, module, persona))).toEqual(new Set([
            'assets/portrait.png', 'assets/joy.png', 'assets/extra.webp', 'assets/inline.png', 'assets/raw.png',
            'assets/backslash.png', 'assets/module.png', 'assets/lore.png', 'assets/icon.png', 'assets/embedded.png',
        ]))
    })
})

describe('collectCharacterConversationAssetKeys', () => {
    it('reads the keys the store finds in the conversations, and none after a failed read or on the web build', async () => {
        core.invoke.mockResolvedValueOnce(['assets/chat.png'])
        await expect(collectCharacterConversationAssetKeys(['char-a'])).resolves.toEqual(['assets/chat.png'])
        expect(core.invoke).toHaveBeenCalledWith('pds_character_asset_keys', { characterIds: ['char-a'] })

        vi.spyOn(console, 'error').mockImplementation(() => {})
        core.invoke.mockRejectedValueOnce(new Error('synthetic read failure'))
        await expect(collectCharacterConversationAssetKeys(['char-a'])).resolves.toEqual([])
        platform.runtime = 'web'
        await expect(collectCharacterConversationAssetKeys(['char-a'])).resolves.toEqual([])
        expect(core.invoke).toHaveBeenCalledTimes(2)
    })
})

describe('scheduleDeletedOwnerAssetCleanup', () => {
    it('commits pending edits, then deletes the assets with the time the delete started', async () => {
        const target = createAssetAliasCleanupTarget()
        core.invoke.mockResolvedValue({ revision: 2 })

        scheduleDeletedOwnerAssetCleanup(['assets/a.png'], 123, 'module-delete', () => target)

        await vi.waitFor(() => expect(core.invoke).toHaveBeenCalledWith('pds_asset_delete_orphaned_aliases', {
            expectedRevision: 1, owner: { keys: ['assets/a.png'], startedAtMs: 123 },
        }))
        expect(target.runtime.flushPendingData).toHaveBeenCalledWith('module-delete')
        expect(target.runtime.flushPendingData.mock.invocationCallOrder[0]).toBeLessThan(core.invoke.mock.invocationCallOrder[0])
    })

    it('leaves the web build and an owner without assets alone', async () => {
        const target = createAssetAliasCleanupTarget()
        scheduleDeletedOwnerAssetCleanup([], 1, 'module-delete', () => target)
        platform.runtime = 'web'
        scheduleDeletedOwnerAssetCleanup(['assets/a.png'], 1, 'module-delete', () => target)
        await Promise.resolve()

        expect(target.runtime.flushPendingData).not.toHaveBeenCalled()
        expect(core.invoke).not.toHaveBeenCalled()
    })

    it('reports a failed commit without deleting', async () => {
        const failure = new Error('synthetic flush failure')
        const logged = vi.spyOn(console, 'error').mockImplementation(() => {})

        scheduleDeletedOwnerAssetCleanup(['assets/a.png'], 1, 'module-delete', () => createAssetAliasCleanupTarget(vi.fn(async () => { throw failure })))

        await vi.waitFor(() => expect(logged).toHaveBeenCalledWith('Deleted owner asset cleanup failed', failure))
        expect(core.invoke).not.toHaveBeenCalled()
    })
})
