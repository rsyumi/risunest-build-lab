import { describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/plugin-dialog', () => ({ save: vi.fn() }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('./persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: vi.fn(),
}))

import { exportNativeCharacterCardFromPicker } from './nativeCharacterCardExportRoute'

describe('native character card export route', () => {
    it('projects JSON metadata from the exact leased revision after destination selection', async () => {
        const calls: unknown[] = []
        const leasedCharacter = {
            name: 'Leased JSON',
            triggerscript: [{ comment: 'trigger' }],
            customscript: [{ comment: 'regex' }],
        }

        await exportNativeCharacterCardFromPicker(
            {
                characterId: 'json-character',
                suggestedName: 'Leased JSON.json',
                format: 'json-card',
                projectCharacter: (character) => {
                    const leased = character as typeof character & typeof leasedCharacter
                    return {
                        spec: 'chara_card_v3',
                        spec_version: '3.0',
                        data: {
                            name: leased.name,
                            extensions: {
                                risuai: {
                                    triggerscript: leased.triggerscript,
                                    customScripts: leased.customscript,
                                },
                            },
                        },
                    }
                },
            },
            {},
            {
                isDesktop: () => true,
                isAndroid: () => false,
                chooseDestination: async (suggestedName, format) => {
                    calls.push(['pick', suggestedName, format])
                    return 'C:\\chosen\\Leased JSON.json'
                },
                runtime: () => ({
                    revision: 42,
                    flushPendingData: async (reason) => { calls.push(['flush', reason]) },
                }),
                readCharacter: async (characterId, revision) => {
                    calls.push(['read', characterId, revision])
                    return leasedCharacter as never
                },
                runExport: async (input) => {
                    calls.push(['export', input])
                    return {
                        revision: 42,
                        sourceBytes: 512,
                        sourceFingerprintKind: 'whole-file-sha256' as const, sourceSha256: 'a'.repeat(64),
                        characterCount: 1,
                        presetCount: 0,
                        warningCodes: [],
                    }
                },
            },
        )

        expect(calls).toEqual([
            ['pick', 'Leased JSON.json', 'json-card'],
            ['flush', 'native-character-card-export'],
            ['read', 'json-character', 42],
            ['export', {
                characterId: 'json-character',
                destination: { type: 'desktopPath', path: 'C:\\chosen\\Leased JSON.json' },
                expectedRevision: 42,
                format: 'json-card',
                metadata: {
                    spec: 'chara_card_v3',
                    spec_version: '3.0',
                    data: {
                        name: 'Leased JSON',
                        extensions: {
                            risuai: {
                                triggerscript: [{ comment: 'trigger' }],
                                customScripts: [{ comment: 'regex' }],
                            },
                        },
                    },
                },
            }],
        ])
        expect(JSON.stringify(calls)).not.toContain('Uint8Array')
        expect(JSON.stringify(calls)).not.toContain('data:image')
    })

    it.each(['success', 'export', 'cancel', 'decode'])('cleans the staged rendition on %s without reopening the picker', async outcome => {
        const owned = new Map([['/owned/rendition', 'png']])
        const controller = new AbortController()
        let destination = 'previous'
        let picks = 0
        const action = exportNativeCharacterCardFromPicker({
            characterId: 'avif', suggestedName: 'card.png', format: 'png-card', projectCharacter: () => ({}),
        }, { signal: controller.signal }, {
            isDesktop: () => true, isAndroid: () => false,
            chooseDestination: async () => { picks++; return '/chosen/card.png' },
            runtime: () => ({ revision: 7, flushPendingData: async () => {} }),
            readCharacter: async () => ({ image: 'assets/old.png' }) as never,
            preparePortrait: async (_character, revision) => {
                expect(revision).toBe(7)
                if (outcome === 'decode') { owned.clear(); throw new Error('decode failed') }
                if (outcome === 'cancel') controller.abort()
                return { descriptor: { sourceHash: 'a'.repeat(64), path: '/owned/rendition' }, release: async () => { owned.clear() } }
            },
            runExport: async input => {
                expect(input.portraitRendition?.sourceHash).toBe('a'.repeat(64))
                expect(owned.get(input.portraitRendition!.path)).toBe('png')
                if (outcome === 'export') throw new Error('export failed')
                destination = 'new card'
                return null
            },
        })
        if (outcome === 'success') await action
        else await expect(action).rejects.toBeDefined()
        expect(picks).toBe(1)
        expect(owned.size).toBe(0)
        expect(destination).toBe(outcome === 'success' ? 'new card' : 'previous')
    })

    it('leaves Web and a cancelled picker on the compatibility path', async () => {
        const input = {
            characterId: 'json-character',
            suggestedName: 'JSON.json',
            format: 'json-card' as const,
            projectCharacter: () => ({ spec: 'chara_card_v3' }),
        }
        const base = {
            isAndroid: () => false,
            runtime: () => ({ revision: 1, flushPendingData: async () => undefined }),
            readCharacter: async () => { throw new Error('must not read') },
            runExport: async () => { throw new Error('must not export') },
        }
        await expect(exportNativeCharacterCardFromPicker(input, {}, {
            ...base,
            isDesktop: () => false,
            chooseDestination: async () => 'unused',
        })).resolves.toBeUndefined()
        await expect(exportNativeCharacterCardFromPicker(input, {}, {
            ...base,
            isDesktop: () => true,
            chooseDestination: async () => null,
        })).resolves.toBeNull()
    })
})
