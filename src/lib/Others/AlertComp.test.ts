// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get } from 'svelte/store'
import { handleRootBack, modalNavigation } from 'src/ts/ui/modalNavigation'
import { alertStore } from 'src/ts/stores.svelte'

const branchMocks = vi.hoisted(() => ({
    getChatBranches: vi.fn(),
}))

vi.mock('src/ts/gui/branches', () => branchMocks)
vi.mock('src/ts/stores.svelte', async () => (await import('./AlertComp.testSupport')).storesModule())
vi.mock('../../ts/alert', async () => (await import('./AlertComp.testSupport')).alertModule())
vi.mock('../../ts/characters', async () => (await import('./AlertComp.testSupport')).charactersModule())
vi.mock('../../ts/parser/parser.svelte', async () => (await import('../../ts/parser/parser.testSupport')).parserStubModule({ ParseMarkdown: vi.fn() }))
vi.mock('src/ts/characterCards', async () => (await import('./AlertComp.testSupport')).characterCardsModule())
vi.mock('src/ts/globalApi.svelte', () => ({
    aiLawApplies: () => false,
    getFetchData: vi.fn(),
    getFetchLogs: () => [],
    openURL: vi.fn(),
}))
vi.mock('src/ts/tokenizer', async () => (await import('./AlertComp.testSupport')).tokenizerModule())
vi.mock('src/ts/gui/colorscheme', async () => (await import('./AlertComp.testSupport')).colorschemeModule())
vi.mock('../../ts/sourcemap', async () => (await import('./AlertComp.testSupport')).sourcemapModule())
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('src/ts/storage/officialAccountMessage', async () => (await import('./AlertComp.testSupport')).officialAccountMessageModule())
vi.mock('@lucide/svelte', async () => (await import('./AlertComp.testSupport')).lucideModule())
vi.mock('../SideBars/BarIcon.svelte', async () => (await import('./AlertComp.testSupport')).componentModule())
vi.mock('../UI/GUI/TextInput.svelte', async () => (await import('./AlertComp.testSupport')).componentModule())
vi.mock('../UI/GUI/Button.svelte', async () => (await import('./AlertComp.testSupport')).componentModule('button'))
vi.mock('../UI/DeferredMarkdown.svelte', async () => (await import('./AlertComp.testSupport')).componentModule())
vi.mock('../UI/GUI/TextAreaInput.svelte', async () => (await import('./AlertComp.testSupport')).componentModule())
vi.mock('../Setting/Pages/Module/ModuleChatMenu.svelte', async () => (await import('./AlertComp.testSupport')).componentModule())
vi.mock('./Help.svelte', async () => (await import('./AlertComp.testSupport')).componentModule())

import { language } from 'src/lang'
import { alertGenerationInfoStore } from '../../ts/alert'
import AlertComp from './AlertComp.svelte'
import versionData from '../../../version.json'
import { appVer } from 'src/ts/appVersion'

function deferred<T>() {
    let resolve!: (value: T) => void
    const promise = new Promise<T>((next) => {
        resolve = next
    })
    return { promise, resolve }
}

function branch(preview: string, x = 0) {
    return {
        x,
        y: 1,
        connectX: -1,
        connectY: -1,
        content: preview,
        preview,
        multiChild: false,
        chatId: 0,
    }
}

describe('AlertComp branch view', () => {
    let mounted: ReturnType<typeof mount> | undefined

    it('returns the PNG format understood by the native card export route', async () => {
        alertStore.set({ type: 'cardexport', msg: '', submsg: '' })
        const target = document.createElement('div')
        document.body.append(target)
        mounted = mount(AlertComp, { target })
        await tick()
        ;[...target.querySelectorAll('button')].find(button => button.textContent === 'Character Card V3')!.click()
        await tick()
        const select = target.querySelector('select')!
        // Happy DOM only implements :checked for inputs, while Svelte also queries options.
        const querySelector = select.querySelector.bind(select)
        vi.spyOn(select, 'querySelector').mockImplementation(selector => selector === ':checked'
            ? [...select.options].find(option => option.selected) ?? null
            : querySelector(selector))
        select.value = [...select.options].find(option => option.textContent === 'PNG')!.value
        select.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()
        ;[...target.querySelectorAll('button')].find(button => button.textContent === language.export)!.click()
        expect(JSON.parse(get(alertStore).msg)).toEqual({ type: '', type2: 'png' })
    })

    it('shows and copies diagnostics for an error without a stack', async () => {
        const writeText = vi.fn(async () => {})
        Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
        alertStore.set({ type: 'error', msg: 'Synthetic string failure' })
        const target = document.createElement('div')
        document.body.append(target)
        mounted = mount(AlertComp, { target })
        await tick()
        ;[...target.querySelectorAll('button')].find(button => button.textContent?.includes(language.showErrorDetails))!.click()
        await tick()
        const report = target.querySelector('pre.stack-trace')!.textContent!
        expect(report).toContain(`RisuNest Version: ${versionData.version} (RisuAI ${appVer})`)
        expect(report).toContain('OS: Windows')
        expect(report).toContain('Device:')
        expect(report).toContain('User-Agent:')
        expect(report).toContain('\n\nSynthetic string failure')
        target.querySelector<HTMLButtonElement>('.stack-trace-copy')!.click()
        await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith(report))
    })

    afterEach(async () => {
        alertStore.set({ type: 'none', msg: '' })
        if (mounted) await unmount(mounted)
        mounted = undefined
        document.body.replaceChildren()
        vi.clearAllMocks()
    })

    it.each(['Escape', 'Back'])('dismisses a mounted confirmation before its modal host on %s', async action => {
        const host = document.createElement('div')
        host.innerHTML = '<button>Open confirmation</button>'
        const target = document.createElement('div')
        document.body.append(host, target)
        const close = vi.fn()
        const navigation = modalNavigation(host, { close })
        await Promise.resolve()
        const opener = host.querySelector('button')!
        opener.focus()
        alertStore.set({ type: 'ask', msg: 'Synthetic confirmation' })
        mounted = mount(AlertComp, { target })
        await tick()
        const confirm = [...target.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === 'YES')!
        confirm.focus()
        let settlements = 0
        const unsubscribe = alertStore.subscribe(value => { if (value.type === 'none') settlements++ })
        if (action === 'Escape') window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }))
        else { history.replaceState(null, ''); window.dispatchEvent(new PopStateEvent('popstate')) }
        await tick()
        expect(get(alertStore)).toEqual({ type: 'none', msg: '' })
        expect(settlements).toBe(1)
        expect(close).not.toHaveBeenCalled()
        expect(document.activeElement).toBe(opener)
        unsubscribe()
        history.replaceState(null, '')
        navigation.destroy()
    })
    it('dismisses a confirmation shown outside any modal host on the Android root Back', async () => {
        // App.svelte listens for the Back that MainActivity offers when no history entry is left.
        window.addEventListener('risunest-root-back', handleRootBack)
        alertStore.set({ type: 'ask', msg: 'Synthetic confirmation' })
        const target = document.createElement('div')
        document.body.append(target)
        mounted = mount(AlertComp, { target })
        await tick()
        expect(target.textContent).toContain('Synthetic confirmation')
        let settlements = 0
        const unsubscribe = alertStore.subscribe(value => { if (value.type === 'none') settlements++ })
        const back = new Event('risunest-root-back', { cancelable: true })
        window.dispatchEvent(back)
        await tick()
        expect(back.defaultPrevented).toBe(true)
        expect(get(alertStore)).toEqual({ type: 'none', msg: '' })
        expect(settlements).toBe(1)
        expect(target.textContent).not.toContain('Synthetic confirmation')
        unsubscribe()
        window.removeEventListener('risunest-root-back', handleRootBack)
    })
    it('cancels its branch scan on close and exposes retry after failure', async () => {
        branchMocks.getChatBranches.mockRejectedValueOnce(new Error('synthetic scan')).mockResolvedValueOnce([])
        alertStore.set({ type: 'branches', msg: 'character' })
        const target = document.createElement('div'); document.body.append(target)
        mounted = mount(AlertComp, { target })
        await vi.waitFor(() => expect(target.textContent).toContain(language.branchLoadFailed))
        ;[...target.querySelectorAll('button')].find(button => button.textContent === language.retry)!.click()
        await vi.waitFor(() => expect(branchMocks.getChatBranches).toHaveBeenCalledTimes(2))
        const signal = branchMocks.getChatBranches.mock.calls[1][1] as AbortSignal
        alertStore.set({ type: 'none', msg: '' }); await tick()
        expect(signal.aborted).toBe(true)
    })

    it('runs a selected action synchronously inside its button click', async () => {
        const onSelect = vi.fn();
        alertStore.set({ type: 'select', msg: '__DISPLAY__Prepared export||Copy||Cancel', onSelect });
        const target = document.createElement('div');
        document.body.appendChild(target);
        mounted = mount(AlertComp, { target });
        await tick();
        const button = [...target.querySelectorAll('button')].find(button => button.textContent === 'Copy')!;
        button.click();
        expect(onSelect).toHaveBeenCalledExactlyOnceWith(0);
    });

    it('ignores a late authoritative branch result after the alert target changes', async () => {
        const firstResult = deferred<ReturnType<typeof branch>[]>()
        branchMocks.getChatBranches.mockImplementation((characterId: string) =>
            characterId === 'char-a'
                ? firstResult.promise
                : Promise.resolve([branch('current B')]),
        )
        alertStore.set({ type: 'branches', msg: 'char-a' })
        const target = document.createElement('div')
        document.body.appendChild(target)
        mounted = mount(AlertComp, { target })
        await vi.waitFor(() => {
            expect(branchMocks.getChatBranches).toHaveBeenCalledWith('char-a', expect.any(AbortSignal))
        })

        alertStore.set({ type: 'branches', msg: 'char-b' })
        await vi.waitFor(() => expect(target.querySelectorAll('[role="table"]')).toHaveLength(1))
        firstResult.resolve([branch('stale A'), branch('stale A second', 1)])
        await firstResult.promise
        await tick()

        const renderedBranches = target.querySelectorAll<HTMLElement>('[role="table"]')
        expect(renderedBranches).toHaveLength(1)
        renderedBranches[0].dispatchEvent(new MouseEvent('mouseenter', { bubbles: true }))
        await tick()
        expect(target.textContent).toContain('current B')
        expect(target.textContent).not.toContain('stale A')
    })

    it('invalidates a late branch result when the alert closes before reopening the same target', async () => {
        const firstResult = deferred<ReturnType<typeof branch>[]>()
        const reopenedResult = deferred<ReturnType<typeof branch>[]>()
        branchMocks.getChatBranches
            .mockReturnValueOnce(firstResult.promise)
            .mockReturnValueOnce(reopenedResult.promise)
        alertStore.set({ type: 'branches', msg: 'char-a' })
        const target = document.createElement('div')
        document.body.appendChild(target)
        mounted = mount(AlertComp, { target })
        await vi.waitFor(() => expect(branchMocks.getChatBranches).toHaveBeenCalledTimes(1))

        alertStore.set({ type: 'none', msg: '' })
        await tick()
        alertStore.set({ type: 'branches', msg: 'char-a' })
        await vi.waitFor(() => expect(branchMocks.getChatBranches).toHaveBeenCalledTimes(2))
        firstResult.resolve([branch('stale closed result')])
        await firstResult.promise
        await tick()

        expect(target.querySelectorAll('[role="table"]')).toHaveLength(0)
        reopenedResult.resolve([branch('fresh reopened result')])
        await vi.waitFor(() => expect(target.querySelectorAll('[role="table"]')).toHaveLength(1))
    })

    it('keeps generation details available after the live conversation is released', async () => {
        const message = 'detached generation message'
        alertGenerationInfoStore.set({
            idx: 42,
            genInfo: {
                model: 'test-model',
                generationId: 'generation-a',
                inputTokens: 3,
                outputTokens: 5,
                maxContext: 100,
            },
            message: {
                role: 'char',
                data: message,
                chatId: 'message-a',
                saying: 'speaker-a',
                time: 123,
            },
        })
        alertStore.set({ type: 'requestdata', msg: 'generation-a' })
        const target = document.createElement('div')
        document.body.appendChild(target)
        mounted = mount(AlertComp, { target })

        const buttons = target.querySelectorAll('button')
        expect(buttons.length).toBeGreaterThanOrEqual(4)
        buttons[1].click()
        await tick()

        expect(target.textContent).toContain('message-a')
        expect(target.textContent).toContain('speaker-a')
        // The token row counts the detached message, and the tokenizer stand-in counts characters.
        expect(target.textContent?.replace(/\s+/g, ' ')).toContain(`Tokens ${message.length}`)
    })
})
