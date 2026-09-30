import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import { DBState } from 'src/ts/stores.svelte'
import ModuleSettings from './ModuleSettings.svelte'
import { importRegex } from 'src/ts/process/scripts'
import { alertConfirm } from 'src/ts/alert'

vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: { modules: [], enabledModules: [] } })
    return { DBState }
})
vi.mock('src/ts/process/modules', () => ({}))
vi.mock('src/ts/gui/tooltip', () => ({ tooltip: () => {} }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(async () => true) }))
vi.mock('src/ts/process/mcp/mcp', () => ({}))
vi.mock('src/ts/interchangeability', () => ({}))
vi.mock('src/ts/characters', () => ({}))
vi.mock('src/lib/SideBars/LoreBook/LoreBookList.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/SideBars/Scripts/RegexList.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/SideBars/Scripts/TriggerList.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/UI/GUI/TextAreaInput.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/Others/Help.svelte', () => ({ default: () => {} }))
vi.mock('src/ts/process/lorebook.svelte', () => ({}))
vi.mock('src/ts/globalApi.svelte', () => ({}))
vi.mock('src/ts/process/scripts', () => ({ importRegex: vi.fn() }))
vi.mock('src/ts/util', () => ({}))

let instance: ReturnType<typeof mount>
beforeEach(() => {
    DBState.db = { modules: [], enabledModules: [] } as any
})
afterEach(async () => {
    if (instance) await unmount(instance)
    document.body.replaceChildren()
})

it('creates one module and keeps the same record when creation finishes', async () => {
    instance = mount(ModuleSettings, { target: document.body })
    document.querySelector<HTMLButtonElement>('button')!.click()
    await tick()
    expect(DBState.db.modules).toHaveLength(1)
    const module = DBState.db.modules[0]
    module.name = 'Synthetic module'
    const finish = [...document.querySelectorAll('button')].find(button => button.textContent === languageEnglish.createModule)!
    finish.click()
    await tick()
    expect(DBState.db.modules).toHaveLength(1)
    expect(DBState.db.modules[0]).toBe(module)
    expect(document.body.textContent?.match(/Synthetic module/g)).toHaveLength(1)
})

async function editSecondModule() {
    DBState.db.modules = ['First', 'Second', 'Third'].map(id => ({ id, name: id, description: '' }))
    instance = mount(ModuleSettings, { target: document.body })
    await tick()
    const row = [...document.querySelectorAll('span')].find(span => span.textContent === 'Second')!.parentElement!
    row.querySelectorAll<HTMLButtonElement>('button')[2].click()
    await tick()
}

it('keeps edits on the current module after the root list is published again', async () => {
    await editSecondModule()
    const name = document.querySelector<HTMLInputElement>('input')!
    name.focus()
    DBState.db.modules = DBState.db.modules.map(module => ({ ...module, description: 'Published' }))
    await tick()
    expect(document.querySelector('input')).toBe(name)
    expect(document.activeElement).toBe(name)
    expect(document.querySelectorAll('input')[1].value).toBe('Published')
    name.value = 'Edited'
    name.dispatchEvent(new Event('input', { bubbles: true }))
    await tick()
    expect(DBState.db.modules[1].name).toBe('Edited')
})

it('does not overwrite another module when an earlier record is removed during editing', async () => {
    await editSecondModule()
    DBState.db.modules.splice(0, 1)
    await tick()
    const finish = [...document.querySelectorAll('button')].find(button => button.textContent === languageEnglish.editModule)!
    finish.click()
    await tick()
    expect(DBState.db.modules.map(module => module.id)).toEqual(['Second', 'Third'])
})

it('closes an editor when its module is removed instead of recreating it', async () => {
    await editSecondModule()
    DBState.db.modules.splice(1, 1)
    await tick()
    expect(document.querySelector('h2')?.textContent).toBe(languageEnglish.modules)
    expect(DBState.db.modules.map(module => module.id)).toEqual(['First', 'Third'])
})

it('keeps concurrent regex edits when an import finishes after publication', async () => {
    let finish!: () => void
    const selected = new Promise<void>(resolve => { finish = resolve })
    vi.mocked(importRegex).mockImplementationOnce(async (scripts = []) => {
        await selected
        scripts.push({ comment: 'Imported', in: '', out: '', type: 'editinput' })
        return scripts
    })
    await editSecondModule()
    const regexTab = [...document.querySelectorAll('button')].find(button => button.textContent?.trim() === languageEnglish.regexScript)!
    regexTab.click()
    await tick()
    Array.from(document.querySelectorAll<HTMLButtonElement>('button.font-medium')).at(-1)!.click()
    DBState.db.modules = DBState.db.modules.map(module => ({ ...module, regex: [
        { comment: 'Concurrent', in: '', out: '', type: 'editinput' },
    ] }))
    await tick()
    finish()
    await vi.waitFor(() => expect(DBState.db.modules[1].regex?.map(script => script.comment)).toEqual(['Concurrent', 'Imported']))
})

it('does not remove the last module when a pending deletion target has already disappeared', async () => {
    let confirm!: (confirmed: boolean) => void
    vi.mocked(alertConfirm).mockReturnValueOnce(new Promise(resolve => { confirm = resolve }))
    DBState.db.modules = ['First', 'Second'].map(id => ({ id, name: id, description: '' }))
    instance = mount(ModuleSettings, { target: document.body })
    await tick()
    const row = [...document.querySelectorAll('span')].find(span => span.textContent === 'First')!.parentElement!
    row.querySelectorAll<HTMLButtonElement>('button')[3].click()
    DBState.db.modules.splice(0, 1)
    await tick()
    confirm(true)
    await tick()
    expect(DBState.db.modules.map(module => module.id)).toEqual(['Second'])
})
