import { expect, it } from 'vitest'
import { languageEnglish } from './en'
import { languageKorean } from './ko'
import { languageChinese } from './cn'
import { languageChineseTraditional } from './zh-Hant'
import { languageGerman } from './de'
import { languageSpanish } from './es'
import { languageVietnamese } from './vi'

it('provides one informational upstream restore warning in all seven languages', () => {
    for (const language of [languageEnglish, languageKorean, languageChinese, languageChineseTraditional, languageGerman, languageSpanish, languageVietnamese]) {
        expect(language.risuNest.importDialog.warningUpstreamRestoreLosses.trim()).not.toBe('')
        const consequence = language.errors.coldStorageIncompleteRestoreConfirm('synthetic', 2, 1, false)
        const defaultConfirmation = language.errors.coldStorageIncompleteRestoreConfirm('synthetic', 2, 1)
        expect(consequence).toContain('synthetic')
        expect(consequence.split('\n\n')).toHaveLength(2)
        expect(defaultConfirmation.startsWith(`${consequence}\n\n`)).toBe(true)
        expect(defaultConfirmation.split('\n\n')).toHaveLength(3)
    }
})

it('uses the approved consequence and action copy', () => {
    expect(languageEnglish.risuNest.importDialog.warningUpstreamRestoreLosses).toBe('Some cold storage or inlay data could not be restored. Review the restored data.')
    expect(languageKorean.risuNest.importDialog.warningUpstreamRestoreLosses).toBe('콜드 스토리지 또는 인레이 데이터 일부를 복원하지 못했습니다. 복원된 데이터를 확인해주세요.')
})
