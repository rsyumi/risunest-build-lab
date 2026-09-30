// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get } from 'svelte/store'
import { alertStore } from 'src/ts/stores.svelte'

const branchMocks = vi.hoisted(() => ({
    getChatBranches: vi.fn(),
}))

vi.mock('src/ts/gui/branches', () => branchMocks)
vi.mock('src/ts/stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return {
        DBState: { db: { characters: [], botPresets: [], botPresetsId: 0 } },
        alertStore: writable({ type: 'none', msg: '' }),
        selectedCharID: writable(-1),
    }
})
vi.mock('../../ts/alert', async () => {
    const { writable } = await import('svelte/store')
    return { alertGenerationInfoStore: writable(null) }
})
vi.mock('../../ts/characters', () => ({ getCharImage: vi.fn() }))
vi.mock('../../ts/parser/parser.svelte', () => ({ ParseMarkdown: vi.fn() }))
vi.mock('src/ts/characterCards', () => ({
    hubURL: '',
    isCharacterHasAssets: () => false,
}))
vi.mock('src/ts/globalApi.svelte', () => ({
    aiLawApplies: () => false,
    getFetchData: vi.fn(),
    getFetchLogs: () => [],
    openURL: vi.fn(),
}))
vi.mock('src/ts/tokenizer', () => ({
    tokenize: vi.fn(async (text: string) => `tokens:${text}`),
}))
vi.mock('src/ts/gui/colorscheme', async () => {
    const { writable } = await import('svelte/store')
    return { ColorSchemeTypeStore: writable(false) }
})
vi.mock('../../ts/sourcemap', () => ({ translateStackTrace: vi.fn() }))
vi.mock('src/ts/platform', () => ({
    isTauri: false,
    getDetailedOSLabel: async () => 'Test OS',
    getFallbackOSLabel: () => 'Test OS',
    getRisuEnvironmentLabel: () => 'Test',
}))
vi.mock('src/ts/storage/officialAccountMessage', () => ({
    isExpectedHubMessage: () => false,
}))
vi.mock('@lucide/svelte', async () => {
    const { default: Stub } = await import('./AlertCompDependencyStub.test.svelte')
    return {
        CheckIcon: Stub,
        ChevronDownIcon: Stub,
        ChevronRightIcon: Stub,
        ChevronUpIcon: Stub,
        CopyIcon: Stub,
        User: Stub,
        XIcon: Stub,
    }
})
vi.mock('../SideBars/BarIcon.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/TextInput.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/Button.svelte', async () => ({
    default: (await import('./AlertCompButtonStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/SelectInput.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/OptionInput.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/DeferredMarkdown.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/TextAreaInput.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../Setting/Pages/Module/ModuleChatMenu.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('./Help.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))

import { changeLanguage } from 'src/lang'
import { languageKorean } from 'src/lang/ko'
import { openURL } from 'src/ts/globalApi.svelte'
import { RISUNEST_PRIVACY_URL, RISUNEST_TERMS_URL, RISU_SERVICE_PRIVACY_URL, RISU_SERVICE_TERMS_URL } from 'src/ts/legal'
import AlertComp from './AlertComp.svelte'

let mounted: ReturnType<typeof mount> | undefined
const text = languageKorean.risuNest.legal
const normalize = (value: string) => value.replace(/\s+/g, ' ').trim()

beforeEach(() => {
    vi.stubEnv('VITE_RISU_LEGAL_CONFIGURED', 'TRUE')
    changeLanguage('ko')
    vi.clearAllMocks()
})

afterEach(async () => {
    alertStore.set({ type: 'none', msg: '' })
    if (mounted) await unmount(mounted)
    mounted = undefined
    document.body.replaceChildren()
    changeLanguage('en')
    vi.unstubAllEnvs()
})

async function show(type: 'tos' | 'risu-tos') {
    alertStore.set({ type, msg: type })
    const target = document.createElement('div')
    document.body.append(target)
    mounted = mount(AlertComp, { target })
    await tick()
    return target
}

function button(target: HTMLElement, label: string): HTMLButtonElement {
    const found = [...target.querySelectorAll('button')].find(item => item.textContent?.trim() === label)
    expect(found, `Missing localized button: ${label}`).toBeDefined()
    return found!
}

describe('Korean legal acceptance dialogs', () => {
    it.each(['tos', 'risu-tos'] as const)('renders %s from the Korean catalog and opens its own legal links', async type => {
        const target = await show(type)
        const own = type === 'tos'
        const terms = own ? text.termsOfUse : text.serviceTerms
        const privacy = own ? text.privacyNotice : text.servicePrivacy
        const expectedBody = (own ? text.tosBody : text.serviceBody)
            .replace('{terms}', terms).replace('{privacy}', privacy)
        expect(normalize(target.textContent ?? '')).toContain(normalize(expectedBody))
        expect(target.textContent).not.toMatch(/\{terms\}|\{privacy\}|I agree|I disagree/)
        expect([...target.querySelectorAll('button')].map(item => item.textContent?.trim()))
            .toEqual([terms, privacy, text.accept, text.decline])
        expect(target.textContent?.includes(text.serviceNotOperated)).toBe(!own)
        button(target, terms).click()
        button(target, privacy).click()
        expect(openURL).toHaveBeenNthCalledWith(1, own ? RISUNEST_TERMS_URL : RISU_SERVICE_TERMS_URL)
        expect(openURL).toHaveBeenNthCalledWith(2, own ? RISUNEST_PRIVACY_URL : RISU_SERVICE_PRIVACY_URL)
        expect(get(alertStore).type).toBe(type)
    })

    it.each([
        ['tos', 'yes'], ['tos', 'no'], ['risu-tos', 'yes'], ['risu-tos', 'no'],
    ] as const)('settles %s with the selected %s action', async (type, answer) => {
        const target = await show(type)
        button(target, answer === 'yes' ? text.accept : text.decline).click()
        await tick()
        expect(get(alertStore)).toEqual({ type: 'none', msg: answer })
        expect(target.querySelector('button')).toBeNull()
    })
})
