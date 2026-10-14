import { describe, expect, it } from 'vitest'
import { language } from 'src/lang'
import { describeBlockedReason } from './blockedReasonText'

describe('describeBlockedReason', () => {
    it('maps native tokens to sentences and hides unknown tokens', () => {
        const text = language.risuNest.serverSync.management
        expect(describeBlockedReason('server-sync-busy')).toBe(text.blockedReasons['server-sync-busy'])
        expect(describeBlockedReason('something-new')).toBe(text.blockedReasonUnknown)
        expect(describeBlockedReason(null)).toBeNull()
        expect(describeBlockedReason('')).toBeNull()
    })
})
