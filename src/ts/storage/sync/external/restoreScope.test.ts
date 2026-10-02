import { describe, expect, it } from 'vitest'
import { externalRestoreAreas, externalRestorableSections } from './restoreScope'

const includedSections = ['hypa', 'local-plugins', 'local-settings'] as const

describe('external full restore scope', () => {
    it('restores all backup sections on both the original and another device', () => {
        for (const sameDevice of [true, false]) {
            const item = { includedSections: [...includedSections], sameDevice }
            expect(externalRestorableSections(item)).toEqual(includedSections)
            expect(externalRestoreAreas(item, [])).toEqual([
                'library', 'referencedAssets', ...includedSections,
            ])
        }
    })

    it('rejects an incomplete backup instead of narrowing its restore', () => {
        expect(() => externalRestoreAreas({ includedSections: ['hypa'], sameDevice: true }, []))
            .toThrow('complete restore scope')
    })
})
