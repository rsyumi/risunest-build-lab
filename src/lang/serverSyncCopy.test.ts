import { describe, expect, it } from 'vitest'
import { languageEnglish } from './en'
import { languageKorean } from './ko'

describe('sync recovery copy', () => {
    it('states replacement without promising an automatic safety copy in either locale', () => {
        expect(languageEnglish.syncConflictRestoreConfirm).toContain('No automatic backup is created')
        expect(languageKorean.syncConflictRestoreConfirm).toContain('자동 백업은 만들지 않습니다')
    })
    it('preserves upstream and paused server questions while using action confirmations elsewhere', () => {
        const found: string[] = []
        const walk = (value: unknown, path: string) => {
            if (typeof value === 'function') value = (value as (name: string) => unknown)('fixture')
            if (typeof value === 'string' && value.includes('까요?')) found.push(path)
            if (value && typeof value === 'object') for (const [key, next] of Object.entries(value)) walk(next, path ? `${path}.${key}` : key)
        }
        walk(languageKorean, '')
        expect(found.sort()).toEqual(['askLoadFirstMsg', 'remindLaterQuestion', 'setup.finally', 'risuNest.serverSync.management.restoreConfirm'].sort())
    })
})
