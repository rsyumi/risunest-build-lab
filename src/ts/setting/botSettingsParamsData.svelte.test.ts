import { afterEach, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'

vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} })
    return { DBState, selIdState: { selId: -1 } }
})
vi.mock('@lucide/svelte', () => ({ TriangleAlert: () => {}, FlaskConicalIcon: () => {}, CircleQuestionMarkIcon: () => {} }))

import { DBState } from 'src/ts/stores.svelte'
import SettingSegmented from 'src/lib/Setting/Wrappers/SettingSegmented.svelte'
import { modelSpecificParameterItems } from './botSettingsParamsData'
import { AnthropicModels } from '../model/providers/anthropic'
import type { Database } from '../storage/database.svelte'
import type { SettingContext } from './types'

let instance: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (instance) await unmount(instance)
    instance = undefined
    document.body.replaceChildren()
})

function item(id: string) {
    return modelSpecificParameterItems.find((entry) => entry.id === id)!
}

it('shows adaptive thinking for an adaptive-only model without rewriting the stored mode', () => {
    DBState.db = { thinkingType: 'budget' } as unknown as Database
    const model = AnthropicModels.find((entry) => entry.id === 'claude-opus-5')!
    const ctx: SettingContext = { db: DBState.db, modelInfo: model, subModelInfo: model }

    instance = mount(SettingSegmented, { target: document.body, props: { item: item('params.thinkingType'), ctx } })
    flushSync()

    const active = [...document.querySelectorAll('.segmented-btn-active')].map((button) => button.textContent?.trim())
    expect(active).toEqual(['Adaptive'])
    expect(item('params.adaptiveThinkingEffort').condition!(ctx)).toBe(true)
    expect(DBState.db.thinkingType).toBe('budget')

    const off = [...document.querySelectorAll<HTMLButtonElement>('.segmented-btn')].find((button) => button.textContent?.trim() === 'Off')!
    off.click()
    flushSync()

    expect(DBState.db.thinkingType).toBe('off')
})
