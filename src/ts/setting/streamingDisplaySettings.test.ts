import { describe, expect, it } from 'vitest'
import { advancedSettingsItems } from './advancedSettingsData'
import { risuNestSettingsItems } from './risuNestSettingsData'

describe('streaming display optimization setting', () => {
    it('offers independent thought and effect controls only in RisuNest settings', () => {
        const thought = risuNestSettingsItems.find(
            (item) => item.id === 'risunest.streaming.thoughtMode',
        )
        const effects = risuNestSettingsItems.find(
            (item) => item.id === 'risunest.streaming.deferEffects',
        )
        expect(thought).toMatchObject({
            type: 'segmented',
            bindKey: 'streamingThoughtMode',
        })
        expect(
            thought?.options?.segmentOptions?.map((option) => option.value),
        ).toEqual(['recent', 'collapsed', 'off'])
        expect(effects).toMatchObject({
            type: 'check',
            bindKey: 'streamingDeferDisplayProcessing',
        })
        for (const item of [thought, effects]) {
            expect(item?.condition).toBeUndefined()
            expect(item?.showExperimental).toBeUndefined()
        }
        expect(
            advancedSettingsItems.some((item) =>
                [
                    'streamingThoughtMode',
                    'streamingDeferDisplayProcessing',
                ].includes(item.bindKey ?? ''),
            ),
        ).toBe(false)
    })
    it('does not expose legacy monotonically growing chat page controls', () => {
        const ids = advancedSettingsItems.map((item) => item.id)
        expect(ids).not.toContain('adv.chatLoadInitial')
        expect(ids).not.toContain('adv.chatLoadAdditional')
    })

    it('keeps the upstream output control in Advanced settings without duplication', () => {
        const setting = advancedSettingsItems.find(
            (item) => item.id === 'adv.streamingDisplayOpt',
        )

        expect(setting).toMatchObject({
            type: 'segmented',
            bindKey: 'streamingDisplayOptimizationMode',
            labelKey: 'streamingDisplayOptimizationMode',
            helpKey: 'streamingDisplayOptimizationMode',
        })
        expect(
            setting?.options?.segmentOptions?.map((option) => option.value),
        ).toEqual(['off', 'balanced', 'strong'])
        expect(
            risuNestSettingsItems.some(
                (item) => item.bindKey === 'streamingDisplayOptimizationMode',
            ),
        ).toBe(false)
        expect(setting?.condition).toBeUndefined()
        expect(setting?.showExperimental).toBeUndefined()
    })
})
