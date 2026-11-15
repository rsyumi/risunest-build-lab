import { getStartupExclusions } from '../storage/deviceSettings'
import type { NativeGenerationHost } from '../storage/nativeGeneration'
import { isStartupExcluded } from '../storage/recoveryMode.svelte'
import { caseUnicode } from './caseUnicode'

export { caseUnicode }

// The languages whose toLocaleLowerCase and toLocaleUpperCase V8 tailors; the probes catch another engine's tailoring
// of the same characters.
const tailoredLanguages = new Set(['az', 'el', 'lt', 'tr'])
const hour = 3_600_000
const day = 24 * hour

/**
 * The Unicode version of the RegExp property and case folding tables: Unicode 17 adds the letter U+10940 and the capital
 * U+A7D2 of U+A7D3, and Unicode 18 the letter U+A7DD, which folds to U+0277.
 */
export function regexUnicode(): NativeGenerationHost['regexUnicode'] {
    if (!/^\p{L}$/u.test('\u{10940}') || !/\u{A7D3}/iu.test('\u{A7D2}')) return 'before17'
    if (/^\p{L}$/u.test('\u{A7DD}') || /\u{277}/iu.test('\u{A7DD}')) return 'from18'
    return 'unicode17'
}

/**
 * Whether `locale`, the default locale when undefined, maps a character the az, tr, lt, el or hy rules tailor other
 * than the root mapping does.
 */
export function caseProbesDiffer(locale?: string): boolean {
    return 'I'.toLocaleLowerCase(locale) !== 'i' || 'i'.toLocaleUpperCase(locale) !== 'I'
        || 'İ'.toLocaleLowerCase(locale) !== 'i̇'
        || 'Ì'.toLocaleLowerCase(locale) !== 'ì'
        || 'ά'.toLocaleUpperCase(locale) !== 'Ά'
        || 'և'.toLocaleUpperCase(locale) !== 'ԵՒ'
}

/** Whether `locale`, the default locale when undefined, maps case differently from the root mapping. */
export function tailoredCase(locale?: string): boolean {
    const language = new Intl.DateTimeFormat(locale).resolvedOptions().locale.split('-')[0].toLowerCase()
    return tailoredLanguages.has(language) || caseProbesDiffer(locale)
}

/**
 * The offsets `Date`'s local getters add from the first of December two years back to 30 days after `now`, sampled
 * each hour and located to the millisecond, so a local new year's eve before `now` is inside. `dateTimeFormat` names
 * months and weekdays through `Intl.DateTimeFormat`, whose default time zone engines take from the same ICU default as
 * `Date`.
 */
export function localTimeZone(now = Date.now()): NativeGenerationHost['timeZone'] {
    const offset = (t: number) => {
        const date = new Date(t)
        return Date.UTC(date.getFullYear(), date.getMonth(), date.getDate(), date.getHours(), date.getMinutes(),
            date.getSeconds(), date.getMilliseconds()) - t
    }
    const start = Date.UTC(new Date(now).getUTCFullYear() - 2, 11, 1)
    const end = now + 30 * day
    const transitions: [number, number][] = [[start, offset(start)]]
    for (let t = start; t < end;) {
        const next = Math.min(t + hour, end)
        if (offset(next) === offset(t)) {
            t = next
            continue
        }
        let low = t
        let high = next
        while (high - low > 1) {
            const middle = Math.floor((low + high) / 2)
            if (offset(middle) === offset(low)) low = middle
            else high = middle
        }
        if (high < end) transitions.push([high, offset(high)])
        t = high
    }
    return { transitions, end }
}

/** The host state the native CBS and regex stages read at reservation. */
export function captureNativeHost(now = Date.now()): NativeGenerationHost {
    return {
        tailoredCase: tailoredCase(), caseUnicode: caseUnicode(), timeZone: localTimeZone(now), regexUnicode: regexUnicode(),
        regexExcluded: isStartupExcluded('regex', getStartupExclusions()),
    }
}
