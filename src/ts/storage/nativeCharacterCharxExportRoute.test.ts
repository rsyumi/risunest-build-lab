import { describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/plugin-dialog', () => ({ save: vi.fn() }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'windows' }))
vi.mock('./persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: vi.fn(),
}))

import { save } from '@tauri-apps/plugin-dialog'
import { get } from 'svelte/store'

import { exportNativeCharacterCharxFromPicker } from './nativeCharacterCharxExportRoute'
import { registerLibraryFileOperationGate } from './libraryFileOperation'
import { nativeFileOperation } from './nativeFileJobManager'

describe('native character CharX export route', () => {
    it('projects metadata from the exact leased revision after destination selection', async () => {
        const calls: unknown[] = []
        const capturedBeforePicker = { name: 'Before picker', desc: 'old description' }
        const leasedCharacter = { name: 'After picker', desc: 'new description' }
        let persistentCharacter = capturedBeforePicker

        const result = await exportNativeCharacterCharxFromPicker(
            {
                characterId: 'current-character',
                suggestedName: 'Current.charx',
                projectCharacter: (character) => {
                    calls.push(['project', character])
                    return {
                        card: { spec: 'chara_card_v3', data: { ...character } },
                        module: { name: `${character.name} Module`, id: 'module-id' },
                    }
                },
            },
            {},
            {
                isDesktop: () => true,
                isAndroid: () => false,
                chooseDestination: async (name) => {
                    calls.push(['pick', name])
                    persistentCharacter = leasedCharacter
                    return 'C:\\chosen\\Current.charx'
                },
                runtime: () => ({
                    revision: 9,
                    flushPendingData: async (reason) => { calls.push(['flush', reason]) },
                }),
                readCharacter: async (characterId, revision) => {
                    calls.push(['read', characterId, revision])
                    return persistentCharacter as never
                },
                runExport: async (input) => {
                    calls.push(['export', input])
                    return {
                        revision: 9,
                        sourceBytes: 1024,
                        sourceFingerprintKind: 'whole-file-sha256' as const, sourceSha256: 'a'.repeat(64),
                        characterCount: 1,
                        presetCount: 0,
                        warningCodes: [],
                    }
                },
            },
        )

        expect(result?.characterCount).toBe(1)
        expect(calls).toEqual([
            ['pick', 'Current.charx'],
            ['flush', 'native-character-charx-export'],
            ['read', 'current-character', 9],
            ['project', leasedCharacter],
            ['export', {
                characterId: 'current-character',
                destination: { type: 'desktopPath', path: 'C:\\chosen\\Current.charx' },
                expectedRevision: 9,
                card: { spec: 'chara_card_v3', data: { ...leasedCharacter } },
                module: { name: 'After picker Module', id: 'module-id' },
            }],
        ])
    })

    it('leaves Web and a cancelled picker on the compatibility path', async () => {
        const runExport = async () => {
            throw new Error('native export must not run')
        }
        const input = {
            characterId: 'current-character',
            suggestedName: 'Current.charx',
            projectCharacter: () => ({
                card: { spec: 'chara_card_v3' },
                module: {},
            }),
        }

        await expect(exportNativeCharacterCharxFromPicker(input, {}, {
            isDesktop: () => false,
            isAndroid: () => false,
            chooseDestination: async () => 'unused',
            runtime: () => ({ revision: 1, flushPendingData: async () => undefined }),
            readCharacter: async () => { throw new Error('must not read') },
            runExport,
        })).resolves.toBeUndefined()

        await expect(exportNativeCharacterCharxFromPicker(input, {}, {
            isDesktop: () => true,
            isAndroid: () => false,
            chooseDestination: async () => null,
            runtime: () => ({ revision: 1, flushPendingData: async () => undefined }),
            readCharacter: async () => { throw new Error('must not read') },
            runExport,
        })).resolves.toBeNull()
    })

    it('waits for sync and backups to settle before opening the picker', async () => {
        const calls: string[] = []
        let settle!: () => void
        const unregister = registerLibraryFileOperationGate(() => new Promise<void>(resolve => { settle = resolve }))
        try {
            const exporting = exportNativeCharacterCharxFromPicker({
                characterId: 'current-character',
                suggestedName: 'Current.charx',
                projectCharacter: () => ({ card: {}, module: {} }),
            }, {}, {
                isDesktop: () => true,
                isAndroid: () => false,
                chooseDestination: async () => { calls.push('pick'); return 'C:\\chosen\\Current.charx' },
                runtime: () => ({ revision: 1, flushPendingData: async () => undefined }),
                readCharacter: async () => ({}) as never,
                runExport: async () => { calls.push('export'); return null as never },
            })
            await vi.waitFor(() => expect(get(nativeFileOperation)?.waitingForSync).toBe(true))
            expect(calls).toEqual([])
            settle()
            await exporting
        } finally { unregister() }
        expect(calls).toEqual(['pick', 'export'])
    })

    it('pins the Android export to the revision after the save picker returns', async () => {
        const calls: unknown[] = []
        const leasedCharacter = { name: 'Android character', desc: 'leased' }
        let revision = 12
        const reservation = {
            requestId: 'synthetic-request',
            bind: async () => undefined,
            release: async () => { calls.push(['release']) },
        }
        const dependencies = {
            isDesktop: () => false,
            isAndroid: () => true,
            chooseDestination: async (): Promise<string | null> => {
                throw new Error('desktop picker must not run')
            },
            reserveAndroidDestination: async (name: string, kind: string) => {
                calls.push(['pick', name, kind])
                // A streaming response commits while the save picker is open.
                revision = 13
                return reservation
            },
            runtime: () => ({
                get revision() { return revision },
                flushPendingData: async (reason: string) => { calls.push(['flush', reason]) },
            }),
            readCharacter: async (_characterId: string, pinned: number) => {
                calls.push(['read', pinned])
                return leasedCharacter as never
            },
            runExport: async (input: unknown) => {
                calls.push(['export', input])
                return {
                    revision: 13,
                    sourceBytes: 10,
                    sourceFingerprintKind: 'whole-file-sha256' as const, sourceSha256: 'a'.repeat(64),
                    characterCount: 1,
                    presetCount: 0,
                    warningCodes: [],
                }
            },
        }
        const input = {
            characterId: 'android-character',
            suggestedName: 'Android character.charx',
            projectCharacter: (character: Record<string, unknown>) => ({
                card: { spec: 'chara_card_v3', data: { ...character } },
                module: {},
            }),
        }

        await exportNativeCharacterCharxFromPicker(input, {}, dependencies)

        expect(calls).toEqual([
            ['pick', 'Android character.charx', 'risuSave'],
            ['flush', 'native-character-charx-export'],
            ['read', 13],
            ['export', {
                characterId: 'android-character',
                destination: { type: 'androidSaf', suggestedName: 'Android character.charx', reservation },
                expectedRevision: 13,
                card: { spec: 'chara_card_v3', data: leasedCharacter },
                module: {},
            }],
            ['release'],
        ])

        // A failure after the picker still releases the picked destination.
        calls.length = 0
        await expect(exportNativeCharacterCharxFromPicker(input, {}, {
            ...dependencies,
            runtime: () => ({ revision, flushPendingData: async () => { throw new Error('flush failed') } }),
        })).rejects.toThrow('flush failed')
        expect(calls).toEqual([['pick', 'Android character.charx', 'risuSave'], ['release']])

        // Backing out of the save picker ends the export without an error, while a cancelled job still rejects.
        const backedOut = {
            ...dependencies,
            reserveAndroidDestination: async (name: string, kind: string) => {
                calls.push(['pick', name, kind])
                throw new DOMException('Android SAF export was cancelled', 'AbortError')
            },
        }
        calls.length = 0
        await expect(exportNativeCharacterCharxFromPicker(input, {}, backedOut)).resolves.toBeNull()
        expect(calls).toEqual([['pick', 'Android character.charx', 'risuSave']])
        await expect(exportNativeCharacterCharxFromPicker(input, { signal: AbortSignal.abort() }, backedOut))
            .rejects.toMatchObject({ name: 'AbortError' })
    })

    it('matches the desktop dialog filter to the requested container', async () => {
        vi.mocked(save).mockResolvedValue(null)
        const base = {
            characterId: 'current-character',
            projectCharacter: () => ({ card: {}, module: {} }),
        }

        await expect(exportNativeCharacterCharxFromPicker({
            ...base,
            suggestedName: 'Current.jpeg',
            container: 'appended-charx-jpeg',
        })).resolves.toBeNull()
        expect(save).toHaveBeenLastCalledWith({
            defaultPath: 'Current.jpeg',
            filters: [{ name: 'CharX JPEG', extensions: ['jpeg'] }],
        })

        await expect(exportNativeCharacterCharxFromPicker({
            ...base,
            suggestedName: 'Current.charx',
        })).resolves.toBeNull()
        expect(save).toHaveBeenLastCalledWith({
            defaultPath: 'Current.charx',
            filters: [{ name: 'CharX', extensions: ['charx'] }],
        })
    })

    it('adds only the appended-JPEG discriminator while plain CharX remains omitted', async () => {
        const inputs: unknown[] = []
        const base = {
            characterId: 'current-character',
            suggestedName: 'Current.jpeg',
            projectCharacter: () => ({
                card: { spec: 'chara_card_v3' },
                module: {},
            }),
        }
        const dependencies = {
            isDesktop: () => false,
            isAndroid: () => true,
            reserveAndroidDestination: async () => ({ requestId: 'synthetic-request', bind: async () => undefined, release: async () => undefined }),
            chooseDestination: async () => null,
            runtime: () => ({ revision: 7, flushPendingData: async () => undefined }),
            readCharacter: async () => ({ name: 'Current' }) as never,
            runExport: async (input: unknown) => {
                inputs.push(input)
                return {
                    revision: 7,
                    sourceBytes: 1,
                    sourceFingerprintKind: 'whole-file-sha256' as const, sourceSha256: 'a'.repeat(64),
                    characterCount: 1,
                    presetCount: 0,
                    warningCodes: [],
                }
            },
        }

        await exportNativeCharacterCharxFromPicker(base, {}, dependencies as never)
        await exportNativeCharacterCharxFromPicker({
            ...base,
            container: 'appended-charx-jpeg',
        }, {}, dependencies as never)

        expect(inputs).toEqual([
            expect.not.objectContaining({ container: expect.anything() }),
            expect.objectContaining({ container: 'appended-charx-jpeg' }),
        ])
    })
})
