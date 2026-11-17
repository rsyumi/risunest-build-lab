import { afterEach, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import { writable } from 'svelte/store'
import { language } from 'src/lang'
import { DBState, onboardingModalStore } from 'src/ts/stores.svelte'
import OnboardingModal from './OnboardingModal.svelte'

const mocks = vi.hoisted(() => ({
    capture: vi.fn(),
    finish: vi.fn(),
    parse: vi.fn(async (html: string, _char: unknown, _registry: unknown) => html),
    mountInlays: vi.fn(),
    releaseInlays: vi.fn(),
    error: vi.fn(),
}))
vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} as any })
    const onboardingModalStore = $state({ open: false })
    return { DBState, onboardingModalStore, selectedCharID: writable(0) }
})
vi.mock('src/ts/chatBindings.svelte', () => ({ captureOnboardingTarget: mocks.capture, finishOnboarding: mocks.finish }))
vi.mock('src/ts/alert', () => ({ alertError: mocks.error }))
vi.mock('src/ts/util', () => ({ parseKeyValue: () => [] }))
vi.mock('src/ts/storage/database.svelte', () => ({}))
vi.mock('src/ts/gui/colorscheme', () => ({ ColorSchemeTypeStore: writable(false) }))
vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule({
    parseOnboardingHTML: mocks.parse,
}))
vi.mock('src/ts/process/files/inlayRenderSource', () => ({
    DeferredInlayMarkerRegistry: class {
        clear() {}
    },
    mountDeferredInlaySources: (root: ParentNode, registry: unknown) => {
        mocks.mountInlays(root, registry)
        return mocks.releaseInlays
    },
}))

const formSetup = {
    enabled: true, mode: 'form', description: 'Pick a start',
    questions: [
        { id: 'q1', title: 'Route', type: 'single', varName: 'route', required: true, options: [{ label: 'Forest', value: 'forest' }, { label: 'Sea', value: '' }] },
        { id: 'q2', title: 'Tags', type: 'multiple', varName: 'tags', options: [{ label: 'X', value: 'x' }, { label: 'Y', value: 'y' }, { label: 'Z', value: 'z' }] },
        { id: 'q3', title: 'Name', type: 'text', varName: 'name', placeholder: 'Your name', options: [] },
        { id: 'q4', title: 'Unbound', type: 'text', varName: '', options: [] },
    ],
}
const htmlSetup = {
    enabled: true, mode: 'html', questions: [],
    html: [
        '<div id="forest" risu-change-var-name="route" risu-change-var-value="forest"><span id="forest-label">Forest</span></div>',
        '<div id="sea" risu-change-var-name="route" risu-change-var-value="sea">Sea</div>',
        '<input id="name" risu-change-var-name="name">',
        '<input id="agree" type="checkbox" risu-change-var-name="agree">',
        '<input id="pick-a" type="radio" name="pick" value="a" risu-change-var-name="pick">',
        '<input id="pick-b" type="radio" name="pick" value="b" risu-change-var-name="pick">',
        '<form id="form"><button id="submit" type="submit">Go</button></form>',
    ].join(''),
}

let instance: ReturnType<typeof mount> | undefined
let target: { characterId: string; conversationId: string; conversation: any; isCurrent: () => boolean }

function open(onboarding: unknown, scriptstate: Record<string, string> = {}, props: Record<string, unknown> = {}) {
    const conversation = { id: 'chat', message: [], scriptstate }
    DBState.db = { characters: [{ chaId: 'character', name: 'Synthetic', chatPage: 0, chats: [conversation], onboarding }] } as any
    target = { characterId: 'character', conversationId: 'chat', conversation, isCurrent: () => true }
    mocks.capture.mockReturnValue(target)
    mocks.finish.mockResolvedValue('applied')
    onboardingModalStore.open = true
    instance = mount(OnboardingModal, { target: document.body, props })
    flushSync()
}
const choice = (label: string) => [...document.querySelectorAll<HTMLButtonElement>('[role="radio"], [role="checkbox"]')]
    .find(button => button.textContent?.trim() === label)!
const button = (label: string) => [...document.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent?.trim() === label)!
const byId = <T extends HTMLElement>(id: string) => document.getElementById(id) as T
function edit(element: HTMLInputElement, event: 'input' | 'change', change: (input: HTMLInputElement) => void) {
    change(element)
    element.dispatchEvent(new Event(event, { bubbles: true }))
}

afterEach(async () => {
    if (instance) await unmount(instance)
    instance = undefined
    document.body.replaceChildren()
    vi.clearAllMocks()
})

it('stages single, multiple and text answers and applies them to the chat on Done', async () => {
    open(formSetup)
    expect(document.body.textContent).not.toContain('Unbound')
    choice('Sea').click()
    choice('Forest').click()
    for (const tag of ['X', 'Y', 'Z', 'Y']) choice(tag).click()
    edit(document.querySelector<HTMLInputElement>('input[placeholder="Your name"]')!, 'input', input => { input.value = 'Mina' })
    flushSync()
    expect(choice('Forest').getAttribute('aria-checked')).toBe('true')
    expect(choice('Sea').getAttribute('aria-checked')).toBe('false')
    expect(['X', 'Y', 'Z'].map(tag => choice(tag).getAttribute('aria-checked'))).toEqual(['true', 'false', 'true'])
    expect(mocks.finish).not.toHaveBeenCalled()
    button(language.onboardingDone).click()
    await vi.waitFor(() => expect(onboardingModalStore.open).toBe(false))
    expect(mocks.finish).toHaveBeenCalledWith(target, new Map([['route', 'forest'], ['tags', '["x","z"]'], ['name', 'Mina']]))
    expect(target.conversation.scriptstate).toEqual({})
})

it('holds Done until every required question is answered', async () => {
    open(formSetup)
    button(language.onboardingDone).click()
    flushSync()
    expect(document.body.textContent).toContain(language.onboardingRequiredMissing)
    expect(mocks.finish).not.toHaveBeenCalled()
    expect(onboardingModalStore.open).toBe(true)
    choice('Sea').click()
    flushSync()
    expect(document.body.textContent).not.toContain(language.onboardingRequiredMissing)
    button(language.onboardingDone).click()
    await vi.waitFor(() => expect(mocks.finish).toHaveBeenCalledWith(target, new Map([['route', 'Sea']])))
})

it('reopens with the answers already stored in the chat', async () => {
    open(formSetup, { $route: 'Sea', $tags: '["y"]', $name: 'Ari' })
    expect(choice('Sea').getAttribute('aria-checked')).toBe('true')
    expect(['X', 'Y', 'Z'].map(tag => choice(tag).getAttribute('aria-checked'))).toEqual(['false', 'true', 'false'])
    expect(document.querySelector<HTMLInputElement>('input[placeholder="Your name"]')!.value).toBe('Ari')
    button(language.onboardingDone).click()
    await vi.waitFor(() => expect(mocks.finish).toHaveBeenCalledWith(target, new Map([['route', 'Sea'], ['tags', '["y"]'], ['name', 'Ari']])))
})

it('skips without writing the staged answers', async () => {
    open(formSetup)
    choice('Forest').click()
    button(language.onboardingSkip).click()
    await vi.waitFor(() => expect(onboardingModalStore.open).toBe(false))
    expect(mocks.finish.mock.calls).toEqual([[target, null]])
})

it('writes to the chat captured when it opened after the selection moves', async () => {
    open(formSetup)
    mocks.capture.mockReturnValue({ ...target, conversationId: 'other', conversation: { id: 'other', message: [] } })
    button(language.onboardingSkip).click()
    await vi.waitFor(() => expect(mocks.finish).toHaveBeenCalledOnce())
    expect(mocks.finish.mock.calls[0][0]).toBe(target)
})

it('stays open while a generation blocks the write or the save fails, and saves once at a time', async () => {
    open(formSetup)
    choice('Forest').click()
    let settle!: (result: string) => void
    mocks.finish.mockImplementationOnce(() => new Promise(resolve => { settle = resolve }))
    button(language.onboardingDone).click()
    button(language.onboardingDone).click()
    await tick()
    expect(button(language.onboardingSkip).disabled).toBe(true)
    expect(mocks.finish).toHaveBeenCalledOnce()
    settle('blocked')
    await vi.waitFor(() => expect(button(language.onboardingDone).disabled).toBe(false))
    expect(onboardingModalStore.open).toBe(true)
    const failure = new Error('Synthetic save failure')
    mocks.finish.mockRejectedValueOnce(failure)
    button(language.onboardingDone).click()
    await vi.waitFor(() => expect(mocks.error).toHaveBeenCalledOnce())
    expect(onboardingModalStore.open).toBe(true)
    mocks.finish.mockResolvedValueOnce('stale')
    button(language.onboardingDone).click()
    await vi.waitFor(() => expect(onboardingModalStore.open).toBe(false))
})

it('previews a draft without reading or writing any chat', async () => {
    const onclose = vi.fn()
    open(formSetup, { $route: 'forest' }, { preview: true, onclose })
    expect(mocks.capture).not.toHaveBeenCalled()
    expect(choice('Forest').getAttribute('aria-checked')).toBe('false')
    choice('Sea').click()
    button(language.onboardingDone).click()
    await vi.waitFor(() => expect(onclose).toHaveBeenCalledOnce())
    expect(mocks.finish).not.toHaveBeenCalled()
    expect(onboardingModalStore.open).toBe(true)
})

it('binds the rendered HTML choices and inputs to chat variables and blocks form submission', async () => {
    open(htmlSetup, { $route: 'sea', $name: 'Ari', $pick: 'b' })
    await vi.waitFor(() => expect(byId('sea').classList.contains('x-risu-selected')).toBe(true))
    expect(mocks.parse).toHaveBeenCalledWith(htmlSetup.html, expect.objectContaining({ chaId: 'character' }), expect.anything())
    const [root, registry] = mocks.mountInlays.mock.calls[0]
    expect((root as HTMLElement).contains(byId('forest'))).toBe(true)
    expect(registry).toBe(mocks.parse.mock.calls[0][2])
    expect(byId('forest').classList.contains('x-risu-selected')).toBe(false)
    expect(byId<HTMLInputElement>('name').value).toBe('Ari')
    expect(byId<HTMLInputElement>('pick-b').checked).toBe(true)

    byId('forest-label').click()
    expect(byId('forest').classList.contains('x-risu-selected')).toBe(true)
    expect(byId('sea').classList.contains('x-risu-selected')).toBe(false)
    edit(byId('name'), 'input', input => { input.value = 'Mina' })
    edit(byId('agree'), 'change', input => { input.checked = true })
    edit(byId('pick-a'), 'change', input => { input.checked = true })
    const submit = new Event('submit', { bubbles: true, cancelable: true })
    byId('form').dispatchEvent(submit)
    expect(submit.defaultPrevented).toBe(true)

    button(language.onboardingDone).click()
    await vi.waitFor(() => expect(mocks.finish).toHaveBeenCalledWith(target, new Map([['route', 'forest'], ['name', 'Mina'], ['agree', '1'], ['pick', 'a']])))
    await unmount(instance!)
    instance = undefined
    expect(mocks.releaseInlays).toHaveBeenCalledOnce()
})
