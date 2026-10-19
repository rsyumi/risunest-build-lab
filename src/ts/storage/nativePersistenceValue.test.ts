import { describe, expect, it, vi } from 'vitest'
import { prepareNativePersistenceValue, UnsaveableValueError } from './nativePersistenceValue'

describe('native persistence Unicode contract', () => {
    it('replaces only malformed surrogates and preserves decomposed text and pairs', () => {
        const diagnostic = vi.spyOn(console, 'warn').mockImplementation(() => {})
        const input = { '\ud800': ['\udc00', 'e\u0301', '\ud83d\ude00'] }
        expect(prepareNativePersistenceValue(input)).toEqual({ '\ufffd': ['\ufffd', 'e\u0301', '😀'] })
        expect(input['\ud800'][0]).toBe('\udc00')
        expect(diagnostic).toHaveBeenCalledExactlyOnceWith('Native persistence replaced invalid Unicode', {
            area: 'persistent data', replacements: 2,
        })
        diagnostic.mockRestore()
    })
    it('does not scan well-formed text for replacements', () => {
        const replace = vi.spyOn(String.prototype, 'replace')
        try {
            const input = { '키': ['hello 😀', 'é'], nested: { text: '한글' } }
            expect(prepareNativePersistenceValue(input)).toEqual(input)
            expect(replace).not.toHaveBeenCalled()
        } finally {
            replace.mockRestore()
        }
    })
    it('still replaces malformed surrogates where the runtime lacks isWellFormed', () => {
        const native = Object.getOwnPropertyDescriptor(String.prototype, 'isWellFormed')
        const diagnostic = vi.spyOn(console, 'warn').mockImplementation(() => {})
        delete (String.prototype as { isWellFormed?: unknown }).isWellFormed
        try {
            expect(prepareNativePersistenceValue({ '\ud800': ['\udc00', 'é', '😀'] }))
                .toEqual({ '�': ['�', 'é', '😀'] })
        } finally {
            if (native) Object.defineProperty(String.prototype, 'isWellFormed', native)
            diagnostic.mockRestore()
        }
    })
    it('rejects replacement key collisions before publishing any value', () => {
        expect(() => prepareNativePersistenceValue({ '\ud800': 1, '\ufffd': 2 }))
            .toThrow('Unicode replacement would merge object keys')
    })
    it('rejects excessive nesting and cycles without native submission', () => {
        let input: unknown = null
        for (let depth = 0; depth < 102; depth++) input = [input]
        expect(() => prepareNativePersistenceValue(input)).toThrow(UnsaveableValueError)
        const cycle: unknown[] = []
        cycle.push(cycle)
        expect(() => prepareNativePersistenceValue(cycle)).toThrow('cyclic value')
    })
    it('preserves dangerous property names as own data', () => {
        const input = JSON.parse('{"__proto__":{"value":1},"constructor":2}')
        expect(JSON.stringify(prepareNativePersistenceValue(input))).toBe(JSON.stringify(input))
    })
    it('reports the affected record without including its content', () => {
        const character = { chaId: 'synthetic-character', privateText: 'private fixture', recursive: null as unknown }
        character.recursive = character
        try {
            prepareNativePersistenceValue({ characterDetails: [character] })
            throw new Error('Expected invalid native value to be rejected')
        } catch (error) {
            expect(error).toMatchObject({
                code: 'unsaveable-value', area: 'character', recordId: 'synthetic-character', reason: 'cyclic value',
            })
            expect(String(error)).not.toContain('private fixture')
        }
    })
    it('validates values produced by JSON hooks before native invocation', () => {
        const diagnostic = vi.spyOn(console, 'warn').mockImplementation(() => {})
        expect(prepareNativePersistenceValue({ toJSON: () => '\ud800' })).toBe('\ufffd')
        diagnostic.mockRestore()
    })
})
