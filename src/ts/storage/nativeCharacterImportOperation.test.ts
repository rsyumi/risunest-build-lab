import { beforeEach, expect, test, vi } from 'vitest'
const observations = vi.hoisted(() => ({ outcomes: [] as string[], operations: 0, onStatus: vi.fn() }))
vi.mock('./contentImportOperation', () => ({ runContentImport: async (_name, _options, operation) => {
    observations.operations++
    try { const result = await operation({ onStatus: observations.onStatus }); observations.outcomes.push('success'); return result }
    catch (error) { observations.outcomes.push(error.code ?? 'failed'); throw error }
} }))
import { importDesktopNativeCharacterPathInOperation } from './nativeCharacterImportOperation'
import { NativeFileJobError } from './nativeFileJobs'
beforeEach(() => { observations.outcomes = []; observations.operations = 0; vi.clearAllMocks() })

test('native unsupported card plus successful fallback settles one successful operation', async () => {
    const nativeImport = vi.fn(async () => { throw new NativeFileJobError('unsupported-format', 'synthetic') })
    const legacyImport = vi.fn(async () => 'character-id')
    const result = await importDesktopNativeCharacterPathInOperation('C:/synthetic.json', { nativeEnabled: () => true, readDesktopPath: async () => Uint8Array.of(1), nativeImport, legacyImport })
    expect(result).toMatchObject({ kind: 'imported', mode: 'legacy', value: 'character-id' })
    expect(nativeImport).toHaveBeenCalledWith(expect.anything(), { onStatus: observations.onStatus })
    expect(observations.operations).toBe(1)
    expect(observations.outcomes).toEqual(['success'])
})

test.each(['unsupported-without-destination', 'store-error'])('native %s settles one failed operation without fallback', async (code) => {
    const legacyImport = vi.fn()
    const result = await importDesktopNativeCharacterPathInOperation('C:/synthetic.jpg', { nativeEnabled: () => true, readDesktopPath: vi.fn(), nativeImport: async () => { throw new NativeFileJobError(code, 'synthetic') }, legacyImport })
    expect(result.kind).toBe('failed')
    expect(legacyImport).not.toHaveBeenCalled()
    expect(observations.operations).toBe(1)
    expect(observations.outcomes).toEqual([code === 'unsupported-without-destination' ? 'destination-required' : code])
})
