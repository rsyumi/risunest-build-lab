import { beforeEach, describe, expect, it, vi } from 'vitest'

const prompt = vi.hoisted(() => ({
    ask: vi.fn(),
    persisted: [] as string[],
    update: vi.fn(),
}))

vi.mock('src/lang', async () => {
    const merge = (await import('lodash/merge')).default
    const { languageEnglish } = await import('../../lang/en')
    const { languageKorean } = await import('../../lang/ko')
    return { language: merge({}, languageEnglish, languageKorean) }
})
vi.mock('../alert', () => ({ alertActionConfirm: prompt.ask }))
vi.mock('./deviceSettings', () => ({
    getStartupExclusions: () => prompt.persisted,
    updateStartupExclusions: prompt.update,
}))

import { languageEnglish } from '../../lang/en'
import { languageKorean } from '../../lang/ko'
import { offerToKeepRecoveryExclusions } from './recoveryExclusionPrompt'

beforeEach(() => {
    prompt.ask.mockReset()
    prompt.update.mockReset()
    prompt.persisted = []
})

describe('offering to keep a start\'s exclusions', () => {
    it('asks with the RisuNest dialog and labeled actions, naming what was left off without a particle placeholder', async () => {
        prompt.ask.mockResolvedValue(false)
        await offerToKeepRecoveryExclusions(['sync'])
        expect(prompt.ask).toHaveBeenCalledExactlyOnceWith({
            title: '앞으로도 꺼 두시겠습니까?',
            description: '이번 시작에서 끈 항목: 동기화',
            actionLabel: '앞으로도 끄기',
            cancelLabel: '이번에만 끄기',
        })
        expect(prompt.update).not.toHaveBeenCalled()
    })
    it('keeps what was left off together with what this device already keeps off', async () => {
        prompt.persisted = ['theme']
        prompt.ask.mockResolvedValue(true)
        await expect(offerToKeepRecoveryExclusions(['sync', 'plugins'])).resolves.toBe(true)
        expect(prompt.ask.mock.calls[0][0].description).toBe('이번 시작에서 끈 항목: 동기화, 플러그인')
        expect(prompt.update).toHaveBeenCalledExactlyOnceWith(['plugins', 'theme', 'sync'])
    })
    it('carries no particle placeholder in either language', () => {
        for (const strings of [languageKorean.risuNest.recovery, languageEnglish.risuNest.recovery]) {
            for (const text of [strings.keepTitle, strings.keepDescription, strings.keepAction]) {
                expect(text).not.toMatch(/\((을|를|이|가|은|는|으)\)/)
            }
        }
    })
})
