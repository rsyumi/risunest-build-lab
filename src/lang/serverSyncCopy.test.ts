import { describe, expect, it } from 'vitest'
import { languageEnglish } from './en'
import { languageKorean } from './ko'

describe('sync recovery copy', () => {
    it('states replacement without promising an automatic safety copy in either locale', () => {
        expect(languageEnglish.syncConflictRestoreConfirm).toContain('No automatic backup is created')
        expect(languageKorean.syncConflictRestoreConfirm).toContain('자동 백업은 만들지 않습니다')
        for (const text of [languageEnglish.risuNest.serverSync, languageKorean.risuNest.serverSync]) {
            expect(text.management.restoreConfirm).not.toMatch(/first backed up|먼저 백업/)
            expect(text.backupHelp).not.toMatch(/first backed up|먼저 백업/)
        }
    })
    it('keeps upstream question wording but uses action confirmations for RisuNest', () => {
        const found: string[] = []
        const walk = (value: unknown, path: string) => {
            if (typeof value === 'function') value = (value as (name: string) => unknown)('fixture')
            if (typeof value === 'string' && value.includes('까요?')) found.push(path)
            if (value && typeof value === 'object') for (const [key, next] of Object.entries(value)) walk(next, path ? `${path}.${key}` : key)
        }
        walk(languageKorean, '')
        expect(found.sort()).toEqual(['askLoadFirstMsg', 'remindLaterQuestion', 'setup.finally'].sort())
    })
})
