import { describe, expect, it } from 'vitest'
import { languageEnglish } from '../../lang/en'
import { languageKorean } from '../../lang/ko'
import { formatCompatibilityBackupReport } from './compatibleBackupReport'
import type { NativeCompatibilityReport } from './nativeFileJobs'

const report: NativeCompatibilityReport = {
    target: 'pocketrisu',
    preserved: [
        {
            code: 'attachment-files',
            items: '9007199254740993',
            bytes: '18446744073709551615',
            affectedConversations: null,
        },
    ],
    converted: [
        {
            code: 'inlay-ids-remapped',
            items: '2',
            bytes: '0',
            affectedConversations: '0',
        },
    ],
    excluded: [],
}

describe('compatibility backup report', () => {
    it('renders exact native decimal values, known zero and unknown distinctly', () => {
        const labels = {
            ...languageEnglish.portableBackup,
            ...languageEnglish.compatibilityBackupReport,
        }
        const text = formatCompatibilityBackupReport(report, labels)
        expect(text).toContain(`${labels.report} (PocketRisu)`)
        expect(text).toContain('9,007,199,254,740,993')
        expect(text).toContain('18,446,744,073,709,551,615 B')
        expect(text).toContain(`${labels.conversations}: ${labels.unknown}`)
        expect(text).toContain(`${labels.conversations}: 0`)
        expect(text).toContain(`${labels.excluded}\n${labels.none}`)
    })

    it('uses translated categories and a human-readable fallback without exposing codes', () => {
        const labels = {
            ...languageKorean.portableBackup,
            ...languageKorean.compatibilityBackupReport,
        }
        const text = formatCompatibilityBackupReport(
            {
                ...report,
                target: 'risuai',
                excluded: [
                    {
                        code: 'future-internal-code',
                        items: '1',
                        bytes: '0',
                        affectedConversations: null,
                    },
                ],
            },
            labels,
        )
        expect(text).toContain(`${labels.report} (RisuAI)`)
        expect(text).toContain(labels.categories['attachment-files'])
        expect(text).toContain(labels.categories['inlay-ids-remapped'])
        expect(text).toContain(labels.otherCategory)
        expect(text).toContain(`${labels.conversations}: ${labels.unknown}`)
        expect(text).not.toContain('future-internal-code')
    })
})
