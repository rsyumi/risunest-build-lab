import { describe, expect, it } from 'vitest'
import { anniversaryYears, getSpecialDay, ordinal } from './specialDay'

const at = (year: number, month: number, day: number) => new Date(year, month - 1, day, 12)

describe('getSpecialDay', () => {
    it('keeps the fixed-date days', () => {
        expect(getSpecialDay(at(2026, 12, 19), 'en')).toBe('christmas')
        expect(getSpecialDay(at(2026, 12, 25), 'en')).toBe('christmas')
        expect(getSpecialDay(at(2026, 12, 26), 'en')).toBeNull()
        expect(getSpecialDay(at(2027, 1, 3), 'en')).toBe('newYear')
        expect(getSpecialDay(at(2027, 1, 4), 'en')).toBeNull()
        expect(getSpecialDay(at(2027, 4, 1), 'en')).toBe('aprilFool')
        expect(getSpecialDay(at(2026, 10, 31), 'en')).toBe('halloween')
    })

    it('starts the anniversary one year after release', () => {
        expect(getSpecialDay(at(2026, 10, 10), 'en')).toBeNull()
        expect(anniversaryYears(at(2026, 10, 10))).toBe(0)
        expect(getSpecialDay(at(2027, 10, 10), 'en')).toBe('anniversary')
        expect(anniversaryYears(at(2027, 10, 10))).toBe(1)
        expect(anniversaryYears(at(2029, 10, 10))).toBe(3)
        expect(anniversaryYears(at(2029, 10, 11))).toBe(0)
    })

    it('finds the harvest moon on lunar 8/15 for Korean and Chinese only', () => {
        expect(getSpecialDay(at(2024, 9, 17), 'ko')).toBe('harvestMoon')
        expect(getSpecialDay(at(2025, 10, 6), 'ko')).toBe('harvestMoon')
        expect(getSpecialDay(at(2026, 9, 25), 'zh')).toBe('harvestMoon')
        expect(getSpecialDay(at(2026, 9, 25), 'zh-Hant')).toBe('harvestMoon')
        expect(getSpecialDay(at(2026, 9, 25), 'en')).toBeNull()
        expect(getSpecialDay(at(2026, 9, 16), 'ko')).toBeNull()
        expect(getSpecialDay(at(2026, 9, 24), 'ko')).toBeNull()
    })
})

describe('ordinal', () => {
    it('formats English ordinals', () => {
        expect([1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 111].map(ordinal))
            .toEqual(['1st', '2nd', '3rd', '4th', '11th', '12th', '13th', '21st', '22nd', '23rd', '101st', '111th'])
    })
})
