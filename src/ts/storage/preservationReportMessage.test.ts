import { afterEach, describe, expect, it } from 'vitest'
import { changeLanguage } from 'src/lang'
import { formatPreservationReport } from './preservationReportMessage'

const report = { files: '3', bytes: '1536', reason: 'not-required-by-library', deletable: true, path: 'C:/RisuNest/preserved' } as const

describe('preserved source notice', () => {
    afterEach(() => changeLanguage('en'))

    it('states the preserved files with a formatted size in Korean', () => {
        changeLanguage('ko')
        expect(formatPreservationReport(report)).toBe(
            '보존한 파일 3개 · 1.5 KiB\n복원한 라이브러리에 필요하지 않은 파일을 원본 보존 영역에 별도 보관했으며, 이 영역에서 삭제할 수 있습니다.\nC:/RisuNest/preserved',
        )
    })

    it('states the preserved files with a formatted size in English', () => {
        changeLanguage('en')
        expect(formatPreservationReport(report)).toBe(
            'Preserved 3 files · 1.5 KiB\nThese files are kept separately because the restored library does not need them. You can delete them from the source preservation area.\nC:/RisuNest/preserved',
        )
    })

    it('never prints a raw byte count with the English unit word', () => {
        changeLanguage('ko')
        const text = formatPreservationReport({ ...report, bytes: '512' })
        expect(text.startsWith('보존한 파일 3개 · 512 B\n')).toBe(true)
        expect(text).not.toContain('bytes')
    })
})
