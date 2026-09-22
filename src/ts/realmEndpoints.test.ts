import { classifyTestRequest } from '../../tests/support/testNetwork'
import { describe, expect, it } from 'vitest'
import { isRealmUrl, REALM_BLOCKED_URL_PATTERNS } from '../../scripts/realmBlocklist.mjs'
import { REALM_HUB_URL, REALM_NIGHTLY_HUB_URL, REALM_SITE_URL } from './realmEndpoints'

describe('isRealmUrl', () => {
    it.each([
        `${REALM_HUB_URL}/realm/search`,
        `${REALM_HUB_URL}/hub/info/some-id`,
        `${REALM_HUB_URL}/hub/realm/upload`,
        `${REALM_HUB_URL}/hub/report`,
        `${REALM_HUB_URL}/hub/remove`,
        `${REALM_HUB_URL}/resource/img.png`,
        `${REALM_HUB_URL}/rs/assets/hash.png`,
        `${REALM_NIGHTLY_HUB_URL}/realm/search`,
        `${REALM_SITE_URL}/upload`,
        `${REALM_SITE_URL}/api/v1/download/dynamic/id?cors=true`,
        `${REALM_SITE_URL}/character/id`,
    ])('treats %s as Realm', (url) => {
        expect(isRealmUrl(url)).toBe(true)
    })

    it.each([
        `${REALM_HUB_URL}/hub/login`,
        `${REALM_HUB_URL}/hub/account/save`,
        `${REALM_HUB_URL}/cryptokey?key=1`,
        `${REALM_HUB_URL}/transformers/model.onnx`,
        `${REALM_HUB_URL}/proxy2`,
        `${REALM_HUB_URL}/kei`,
        `${REALM_HUB_URL}/redirect/docs/lua`,
        'https://risuai.xyz/',
        'https://example.com/realm/search',
    ])('keeps %s reachable', (url) => {
        expect(isRealmUrl(url)).toBe(false)
    })

    it('does not treat relative or non-http inputs as Realm', () => {
        expect(isRealmUrl('/realm/search')).toBe(false)
        expect(isRealmUrl('blob:https://example.com/uuid')).toBe(false)
        expect(isRealmUrl('data:text/plain,realm')).toBe(false)
        expect(isRealmUrl('')).toBe(false)
    })

    it('accepts URL objects', () => {
        expect(isRealmUrl(new URL(`${REALM_SITE_URL}/upload`))).toBe(true)
    })
})

describe('REALM_BLOCKED_URL_PATTERNS', () => {
    it('stays consistent with isRealmUrl', () => {
        for (const pattern of REALM_BLOCKED_URL_PATTERNS) {
            expect(pattern).toMatch(/^\*:\/\/.+\*$/)
            const sample = 'https://' + pattern.slice('*://'.length, -1) + 'sample'
            expect(isRealmUrl(sample), pattern).toBe(true)
        }
    })
})

describe('Realm request policy inputs', () => {
    it('rejects strings, URLs and Requests before dispatch', () => {
        for (const input of [
            `${REALM_HUB_URL}/realm/search`,
            new URL(`${REALM_SITE_URL}/upload`),
            new Request(`${REALM_HUB_URL}/hub/info/id`),
        ]) expect(classifyTestRequest(input, new Set())).toContain('RisuRealm')
    })
})
