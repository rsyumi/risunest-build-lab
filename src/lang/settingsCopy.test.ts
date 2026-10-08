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
    for (const [path, value] of leaves(languageKorean)) {
        let text: unknown = value
        if (typeof value === 'function') {
            try { text = value('Synthetic', 'Synthetic', 'Synthetic') } catch { continue }
        }
        if (typeof text === 'string' && text.includes('까요?')) expect(inherited.has(path), path).toBe(true)
    }
})
it('describes only the reset and restore actions in their help', () => {
    expect(languageKorean.risuNest.cleanup.scope).toBe('이 기기의 대화, 캐릭터, 설정, 플러그인, 연결 정보, 내부 백업과 캐시를 삭제합니다.')
    expect(languageEnglish.risuNest.cleanup.scope).toBe('Deletes local chats, characters, settings, plugins, connection information, internal backups and cache on this device.')
    expect(languageKorean.risuNest.backup.restoreHelp).toBe('계정 동기화 충돌 백업으로 데이터를 복원합니다.')
    expect(languageEnglish.risuNest.backup.restoreHelp).toBe('Restores data from an account sync conflict backup.')
})
it('names toggle presets without a particle placeholder', () => {
    expect(languageKorean.togglePresetRenamed('기본', '전투')).toBe('"기본" 프리셋 이름을 바꿨으며, 새 이름은 "전투"입니다.')
    expect(languageKorean.togglePresetDuplicated('기본 사본')).toBe('"기본 사본" 프리셋으로 복제했습니다.')
    for (const text of [languageKorean.togglePresetRenamed('A', 'B'), languageKorean.togglePresetDuplicated('B')]) expect(text).not.toMatch(/\((으|이|을|를|은|는|과|와)\)|[을은이과와]\([를는가와과]\)/)
})
