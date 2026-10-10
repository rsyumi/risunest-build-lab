export type SpecialDay = 'christmas' | 'newYear' | 'aprilFool' | 'anniversary' | 'harvestMoon' | 'halloween' | null

export const RISUNEST_RELEASE_DATE = { year: 2026, month: 10, day: 10 } as const

const HARVEST_MOON_LANGUAGES = new Set(['ko', 'zh', 'zh-Hant'])

let lunarFormatter: Intl.DateTimeFormat | null | undefined

function lunarMonthDay(date: Date): string | null {
    if (lunarFormatter === undefined) {
        try {
            lunarFormatter = new Intl.DateTimeFormat('en-u-ca-chinese', { month: 'numeric', day: 'numeric' })
        } catch {
            lunarFormatter = null
        }
    }
    if (!lunarFormatter) return null
    const parts = lunarFormatter.formatToParts(date)
    const month = parts.find((part) => part.type === 'month')?.value
    const day = parts.find((part) => part.type === 'day')?.value
    // A leap month formats as "8bis" and is not the festival month.
    return month && day ? `${month}/${day}` : null
}

export function anniversaryYears(date: Date): number {
    const years = date.getFullYear() - RISUNEST_RELEASE_DATE.year
    const sameDay = date.getMonth() + 1 === RISUNEST_RELEASE_DATE.month && date.getDate() === RISUNEST_RELEASE_DATE.day
    return sameDay && years >= 1 ? years : 0
}

export function getSpecialDay(date: Date, language: string | undefined): SpecialDay {
    const month = date.getMonth() + 1
    const day = date.getDate()
    if (month === 12 && day >= 19 && day <= 25) return 'christmas'
    if (month === 1 && day <= 3) return 'newYear'
    if (month === 4 && day === 1) return 'aprilFool'
    if (month === 10 && day === 31) return 'halloween'
    if (anniversaryYears(date) > 0) return 'anniversary'
    if (HARVEST_MOON_LANGUAGES.has(language ?? '') && (month === 9 || month === 10) && lunarMonthDay(date) === '8/15') {
        return 'harvestMoon'
    }
    return null
}

export function ordinal(num: number): string {
    const lastTwo = num % 100
    if (lastTwo >= 11 && lastTwo <= 13) return `${num}th`
    switch (num % 10) {
        case 1: return `${num}st`
        case 2: return `${num}nd`
        case 3: return `${num}rd`
        default: return `${num}th`
    }
}
