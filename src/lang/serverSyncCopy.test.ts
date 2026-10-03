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
    it('offers a download before disconnecting from a server that keeps files only there', () => {
        expect(languageKorean.risuNest.serverSync).toMatchObject({
            disconnect: '연결 해제',
            disconnectTitle: '연결을 해제하시겠습니까?',
            disconnectRemoteOnly: '서버에만 있는 파일이 있습니다. 필요한 경우 다운로드한 뒤 연결을 해제하세요.',
            downloadThenDisconnect: '다운로드 후 연결 해제',
            downloadFailedKeptConnection: '파일을 다운로드하지 못해 연결을 해제하지 않았습니다. 다시 시도하거나 다운로드하지 않고 연결을 해제하세요.',
            residency: { download: '다운로드' },
        })
        expect(languageEnglish.risuNest.serverSync).toMatchObject({
            disconnect: 'Disconnect',
            disconnectTitle: 'Disconnect from the server?',
            disconnectRemoteOnly: 'Some files are stored only on the server. If you need them, download them before disconnecting.',
            downloadThenDisconnect: 'Download, then disconnect',
            downloadFailedKeptConnection: 'The files could not be downloaded, so the server was not disconnected. Try again, or disconnect without downloading.',
            residency: { download: 'Download' },
        })
        for (const text of [languageEnglish.risuNest.serverSync, languageKorean.risuNest.serverSync]) expect(text).not.toHaveProperty('downloadBeforeDisconnect')
    })
})
