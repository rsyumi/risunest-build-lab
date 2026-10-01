import { expect, it } from 'vitest'
import { languageEnglish } from './en'
import { languageKorean } from './ko'
function leaves(value: unknown, prefix = ''): Map<string, unknown> {
    const result = new Map<string, unknown>()
    for (const [key, entry] of Object.entries(value as object)) {
        const path = prefix ? `${prefix}.${key}` : key
        if (entry && typeof entry === 'object') for (const [child, leaf] of leaves(entry, path)) result.set(child, leaf)
        else result.set(path, entry)
    }
    return result
}
it('keeps RisuNest Korean and English keys aligned', () => {
    expect([...leaves(languageKorean.risuNest).keys()].sort()).toEqual([...leaves(languageEnglish.risuNest).keys()].sort())
    expect(languageKorean.pluginProviderPermissionDenied).toBe('플러그인 모델 사용 권한이 거부되었습니다.')
})
it('uses the required question form for owned Korean confirmations', () => {
    const inherited = new Set(['setup.finally', 'remindLaterQuestion', 'askLoadFirstMsg'])
    expect(languageKorean.risuNest.serverSync.management.restoreConfirm).toBe(
        '이 백업으로 복원하시겠습니까? 현재 라이브러리는 이 백업의 내용으로 교체되며, 동기화는 일시 중지됩니다. 자동 백업은 만들지 않습니다.',
    )
    for (const [path, value] of leaves(languageKorean)) {
        let text: unknown = value
        if (typeof value === 'function') {
            try { text = value('Synthetic', 'Synthetic', 'Synthetic') } catch { continue }
        }
        if (typeof text === 'string' && text.includes('까요?')) expect(inherited.has(path), path).toBe(true)
    }
})
