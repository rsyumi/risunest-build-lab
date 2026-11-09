import { afterEach, describe, expect, it, vi } from 'vitest'
import { updateStartupExclusions } from '../storage/deviceSettings'
import { captureNativeHost, caseProbesDiffer, caseUnicode, localTimeZone, regexUnicode, tailoredCase } from './nativeHostCapture'

const hour = 3_600_000
const zone = process.env.TZ

afterEach(() => {
    vi.restoreAllMocks()
    if (zone === undefined) delete process.env.TZ
    else process.env.TZ = zone
})

describe('native host capture', () => {
    it('reports the Unicode version of the engine case mapping from the capital probes', () => {
        const upper = String.prototype.toUpperCase
        const lower = String.prototype.toLowerCase
        const probes = ['\u{10D70}', '\u{A7D3}', '\u{277}', '\u{A7DD}']
        // What each engine generation maps the probes to: U+10D70 gains its capital in Unicode 16, U+A7D3 in 17, and
        // U+0277 in 18, which adds U+A7DD.
        const v16 = { '\u{10D70}': '\u{10D50}' }
        const v17 = { ...v16, '\u{A7D3}': '\u{A7D2}' }
        const engines: [string, Record<string, string>, Record<string, string>][] = [['before16', {}, {}], ['unicode16', v16, {}],
            ['unicode17', v17, {}], ['from18', { ...v17, '\u{277}': '\u{A7DD}' }, { '\u{A7DD}': '\u{277}' }],
            ['from18', { ...v17, '\u{277}': '\u{A7DD}' }, {}], ['from18', v17, { '\u{A7DD}': '\u{277}' }]]
        for (const [row, [version, capitals, smalls]] of engines.entries()) {
            const mapped = (original: (this: string) => string, table: Record<string, string>) => function (this: string) {
                const value = String(this)
                return probes.includes(value) ? table[value] ?? value : original.call(value)
            }
            vi.spyOn(String.prototype, 'toUpperCase').mockImplementation(mapped(upper, capitals))
            vi.spyOn(String.prototype, 'toLowerCase').mockImplementation(mapped(lower, smalls))
            expect(caseUnicode(), `row ${row}`).toBe(version)
            vi.restoreAllMocks()
        }
    })

    it('reports the Unicode version of the engine RegExp tables from the letter and case folding probes', () => {
        const test = RegExp.prototype.test
        const key = (pattern: RegExp, input: string) => `/${pattern.source}/${pattern.flags} ${input}`
        // Unicode 17 adds the letter U+10940 and the capital U+A7D2 of U+A7D3, and Unicode 18 the letter U+A7DD, which folds to U+0277.
        const letter17 = key(/^\p{L}$/u, '\u{10940}')
        const fold17 = key(/\u{A7D3}/iu, '\u{A7D2}')
        const letter18 = key(/^\p{L}$/u, '\u{A7DD}')
        const fold18 = key(/\u{277}/iu, '\u{A7DD}')
        const probes = [letter17, fold17, letter18, fold18]
        const engines: [string, string[]][] = [['before17', []], ['before17', [letter17]], ['before17', [fold17]],
            ['unicode17', [letter17, fold17]], ['from18', [letter17, fold17, letter18]], ['from18', [letter17, fold17, fold18]]]
        for (const [row, [version, matches]] of engines.entries()) {
            vi.spyOn(RegExp.prototype, 'test').mockImplementation(function (this: RegExp, input: string) {
                const probe = key(this, String(input))
                return probes.includes(probe) ? matches.includes(probe) : test.call(this, input)
            })
            expect(regexUnicode(), `row ${row}`).toBe(version)
            vi.restoreAllMocks()
        }
    })

    it('captures whether this start excludes regex', () => {
        try {
            updateStartupExclusions([])
            expect(captureNativeHost().regexExcluded).toBe(false)
            updateStartupExclusions(['regex'])
            expect(captureNativeHost().regexExcluded).toBe(true)
        } finally {
            updateStartupExclusions([])
        }
    })

    it('reports the locales whose case mapping differs from the root mapping', () => {
        for (const locale of ['tr-TR', 'az', 'lt-LT', 'el']) {
            expect(caseProbesDiffer(locale), locale).toBe(true)
            expect(tailoredCase(locale), locale).toBe(true)
        }
        for (const locale of ['en-US', 'ko-KR', 'de']) {
            expect(caseProbesDiffer(locale), locale).toBe(false)
            expect(tailoredCase(locale), locale).toBe(false)
        }
    })

    it('records each offset change of the local zone to the millisecond, from December two years back to a month ahead', () => {
        const now = Date.UTC(2026, 0, 2, 0, 4, 5, 678)
        const end = now + 720 * hour
        process.env.TZ = 'America/New_York'
        expect(localTimeZone(now)).toEqual({ transitions: [[Date.UTC(2024, 11, 1), -5 * hour], [Date.UTC(2025, 2, 9, 7), -4 * hour],
            [Date.UTC(2025, 10, 2, 6), -5 * hour]], end })
        process.env.TZ = 'Australia/Lord_Howe'
        expect(localTimeZone(now)).toEqual({ transitions: [[Date.UTC(2024, 11, 1), 11 * hour], [Date.UTC(2025, 3, 5, 15), 10.5 * hour],
            [Date.UTC(2025, 9, 4, 15, 30), 11 * hour]], end })
        process.env.TZ = 'Asia/Kathmandu'
        expect(localTimeZone(now)).toEqual({ transitions: [[Date.UTC(2024, 11, 1), 5.75 * hour]], end })
    })
})
