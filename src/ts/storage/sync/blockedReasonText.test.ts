import { describe, expect, it, vi } from 'vitest'

vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))

import { describeBlockedReason } from './blockedReasonText'
import { languageEnglish } from 'src/lang/en'

describe('describeBlockedReason', () => {
    it('maps native tokens to sentences and hides unknown tokens', () => {
        const text = languageEnglish.risuNest.serverSync.management
        expect(describeBlockedReason('backup-in-use')).toBe(text.blockedReasons['backup-in-use'])
        expect(describeBlockedReason('server-sync-busy')).toBe(text.blockedReasons['server-sync-busy'])
        expect(describeBlockedReason('something-new')).toBe(text.blockedReasonUnknown)
        expect(describeBlockedReason(null)).toBeNull()
        expect(describeBlockedReason('')).toBeNull()
    })
})
