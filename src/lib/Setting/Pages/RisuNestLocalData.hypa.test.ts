// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, test, vi } from 'vitest'
import { mount, unmount } from 'svelte'
const mocks = vi.hoisted(() => ({ invoke: vi.fn(), confirm: vi.fn(), error: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('src/ts/platform', () => ({ isTauri: true }))
vi.mock('src/ts/alert', () => ({ alertConfirm: mocks.confirm, alertError: mocks.error }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))
vi.mock('src/ts/storage/localDataSections', () => ({ readLocalDataParticipation: async () => [], setLocalDataParticipating: vi.fn() }))
vi.mock('src/ts/storage/localDataRemotes', () => ({ readLocalDataRemoteState: async () => 'none' }))
import RisuNestLocalData from './RisuNestLocalData.svelte'
let component: ReturnType<typeof mount>
let target: HTMLDivElement
beforeEach(() => {
    vi.clearAllMocks()
    mocks.confirm.mockResolvedValue(true)
    let cleared = false
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'pds_clear_hypa_embeddings') { cleared = true; return }
        if (command === 'pds_hypa_embedding_usage') return cleared ? { count: 0, bytes: 0 } : { count: 2, bytes: 4096 }
        throw new Error('unexpected command')
    })
    target = document.createElement('div')
    document.body.append(target)
    component = mount(RisuNestLocalData, { target })
})
afterEach(async () => { await unmount(component); target.remove() })
function deleteButton() { return [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === 'Remove')! }
test('shows real usage, confirms deletion and refreshes to zero', async () => {
    await vi.waitFor(() => expect(target.textContent).toContain('2 embeddings · 4.0 KiB'))
    deleteButton().click()
    await vi.waitFor(() => expect(target.textContent).toContain('0 embeddings · 0 B'))
    expect(mocks.confirm).toHaveBeenCalledWith(expect.stringContaining('Synced devices will also delete'))
    expect(deleteButton().disabled).toBe(true)
})
test('declining keeps the cache untouched', async () => {
    mocks.confirm.mockResolvedValue(false)
    await vi.waitFor(() => expect(deleteButton()).toBeDefined())
    deleteButton().click()
    await vi.waitFor(() => expect(mocks.confirm).toHaveBeenCalledOnce())
    expect(mocks.invoke).not.toHaveBeenCalledWith('pds_clear_hypa_embeddings')
    expect(target.textContent).toContain('2 embeddings')
})
