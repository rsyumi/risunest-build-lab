import { expect, it } from 'vitest'
import { languageEnglish } from './en'
import { languageKorean } from './ko'
import { languageChinese } from './cn'
import { languageChineseTraditional } from './zh-Hant'
import { languageGerman } from './de'
import { languageSpanish } from './es'
import { languageVietnamese } from './vi'

it('provides all binding, restore and status copy in every language', () => {
    for (const language of [languageKorean, languageChinese, languageChineseTraditional, languageGerman, languageSpanish, languageVietnamese]) {
        expect(Object.keys(language.lwwSync).sort()).toEqual(Object.keys(languageEnglish.lwwSync).sort())
        for (const text of Object.values(language.lwwSync)) expect(text.length).toBeGreaterThan(0)
    }
})
it('preserves the exact approved Korean replacement and restore copy', () => {
    expect(languageKorean.lwwSync).toEqual({
        concurrentEditNotice: '같은 항목을 여러 기기에서 동시에 수정할 경우 마지막 수정 내용이 유지됩니다. 수정한 내용이나 메시지가 사라질 수 있으니 여러 기기를 동시에 사용하지 마세요.',
        replaceTitle: '이 기기의 데이터를 교체하시겠습니까?',
        replaceDescription: '동기화를 위해 이 기기의 데이터를 초기화한 후 원격 데이터로 교체합니다. 백업이 필요한 경우 수동으로 백업해주세요.',
        replaceAcknowledge: '이 기기의 데이터 초기화', replaceAction: '교체', cancelAction: '취소',
        clockBlocked: '기기와 원격 간의 시간 차이가 있어 동기화가 중단되었습니다. 시간을 보정한 후 다시 시도해주세요.',
        writerCollision: '중복된 기기로 인해 동기화가 중단되었습니다. 새 기기로 다시 연결해주세요.',
        unitTooLarge: '이 기기에 서버로 보내기에 너무 큰 항목이 있어 동기화가 중단되었습니다. 해당 항목의 크기를 줄인 후 다시 시도해주세요.',
        newDeviceAction: '새 기기로 연결',
        restoreTitle: '백업을 복원하시겠습니까?',
        restoreDescriptionBound: '현재 데이터를 초기화한 후 선택한 백업으로 복원하며, 복원한 내용은 원격으로 동기화됩니다. 백업이 필요한 경우 수동으로 백업해주세요.',
        restoreDescription: '현재 데이터를 초기화한 후 선택한 백업으로 복원합니다. 백업이 필요한 경우 수동으로 백업해주세요.',
        restoreAcknowledge: '현재 데이터 초기화', restoreAction: '복원',
    })
})
it('preserves the exact approved English new-device action', () => {
    expect(languageEnglish.lwwSync.newDeviceAction).toBe('Connect as new device')
})
