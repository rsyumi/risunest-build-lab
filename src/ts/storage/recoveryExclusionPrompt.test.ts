import { beforeEach, describe, expect, it, vi } from 'vitest'

const prompt = vi.hoisted(() => ({
    ask: vi.fn(),
    persisted: [] as string[],
    update: vi.fn(),
}))

vi.mock('../alert', () => ({ alertActionConfirm: prompt.ask }))
vi.mock('./deviceSettings', () => ({
    getStartupExclusions: () => prompt.persisted,
    updateStartupExclusions: prompt.update,
}))

import { language } from 'src/lang'
import { offerToKeepRecoveryExclusions } from './recoveryExclusionPrompt'

const strings = language.risuNest.recovery

beforeEach(() => {
    prompt.ask.mockReset()
    prompt.update.mockReset()
    prompt.persisted = []
})

describe('offering to keep a start\'s exclusions', () => {
    it('asks with the RisuNest dialog and labeled actions, naming what was left off', async () => {
        prompt.ask.mockResolvedValue(false)
        await offerToKeepRecoveryExclusions(['sync'])
        expect(prompt.ask).toHaveBeenCalledExactlyOnceWith({
            title: strings.keepTitle,
            description: strings.keepDescription.replace('{0}', strings.excludeSync),
            actionLabel: strings.keepAction,
            cancelLabel: strings.excludeTitle,
        })
        expect(prompt.update).not.toHaveBeenCalled()
    })
    it('keeps what was left off together with what this device already keeps off', async () => {
        prompt.persisted = ['theme']
        prompt.ask.mockResolvedValue(true)
        await expect(offerToKeepRecoveryExclusions(['sync', 'plugins'])).resolves.toBe(true)
        expect(prompt.ask.mock.calls[0][0].description).toBe(strings.keepDescription.replace('{0}', `${strings.excludeSync}, ${strings.excludePlugins}`))
        expect(prompt.update).toHaveBeenCalledExactlyOnceWith(['plugins', 'theme', 'sync'])
    })
})
