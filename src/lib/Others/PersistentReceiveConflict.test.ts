// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { fromStore, writable } from 'svelte/store'
import ConflictDialog from './PersistentReceiveConflict.svelte'
import { languageEnglish } from 'src/lang/en'

type Conflict = { id: string; kind: 'history-conflict' | 'history-budget' | 'archived-owner'; canSaveSeparateCopy: boolean; operationError?: unknown }
type Outcome = { kind: 'resolved' | 'conflict-changed' | 'committed-refresh-required' }
const copy = languageEnglish.risuNest.receiveConflict
const conflict = writable<Conflict | null>(null)
const reactive = fromStore(conflict)
const resolve = vi.fn<(request: { id: string; action: 'save-separate-copy' | 'discard-covered-edits' }) => Promise<Outcome>>()
let component: ReturnType<typeof mount>
function button(label: string): HTMLButtonElement {
    return Array.from(document.querySelectorAll('button')).find(button => button.textContent?.replace(languageEnglish.loading, '').trim() === label)!
}
async function show(update: Partial<Conflict> = {}): Promise<void> {
    conflict.set({ id: 'synthetic-first', kind: 'history-conflict', canSaveSeparateCopy: true, ...update })
    await tick()
    await tick()
}
beforeEach(() => {
    conflict.set(null)
    resolve.mockReset()
    component = mount(ConflictDialog, { target: document.body, props: { get conflict() { return reactive.current }, resolve } })
})
afterEach(async () => { await unmount(component); document.body.replaceChildren() })

it('opens only for an unsaved conflict, focuses it, and exposes no internal details', async () => {
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    await show({ id: 'private-id', operationError: new Error('private-payload') })
    expect(document.body.textContent).toContain(copy.unsaved)
    expect(document.body.textContent).toContain(copy.failed)
    expect(document.body.textContent).not.toContain('private')
    expect(document.body.textContent).not.toContain(languageEnglish.risuNest.persistentData.refreshHelp)
    expect(document.activeElement).toBe(document.querySelector('[role="dialog"]'))
    expect(resolve).not.toHaveBeenCalled()
})
it.each(['history-budget', 'archived-owner'] as const)('offers only supported actions for %s', async kind => {
    await show({ kind, canSaveSeparateCopy: false })
    expect(button(copy.saveCopy)).toBeUndefined()
    expect(document.body.textContent).not.toContain(copy.copyHelp)
    expect(button(copy.discard)).toBeDefined()
    expect(document.body.textContent).toContain(kind === 'archived-owner' ? copy.archived : copy.unsaved)
})
it('requires confirmation for discarding edits and allows cancellation', async () => {
    await show()
    button(copy.discard).click(); await tick()
    expect(document.body.textContent).toContain(copy.discardConfirm)
    expect(resolve).not.toHaveBeenCalled()
    button(languageEnglish.cancel).click(); await tick()
    expect(document.body.textContent).not.toContain(copy.discardConfirm)
    button(copy.discard).click(); await tick()
    resolve.mockResolvedValueOnce({ kind: 'resolved' })
    button(copy.discard).click(); await tick(); await tick()
    expect(resolve).toHaveBeenCalledWith({ id: 'synthetic-first', action: 'discard-covered-edits' })
})
it('saves a new character, disables all actions in flight, and waits for the owner to clear recovery', async () => {
    let finish!: (outcome: Outcome) => void
    resolve.mockImplementationOnce(() => new Promise<Outcome>(done => { finish = done }))
    await show()
    button(copy.saveCopy).click(); await tick()
    expect(Array.from(document.querySelectorAll('button')).every(button => button.disabled)).toBe(true)
    expect(resolve).toHaveBeenCalledWith({ id: 'synthetic-first', action: 'save-separate-copy' })
    finish({ kind: 'committed-refresh-required' }); await tick(); await tick()
    expect(document.querySelector('[role="dialog"]')).not.toBeNull()
    expect(Array.from(document.querySelectorAll('button')).every(button => button.disabled)).toBe(true)
    expect(document.body.textContent).not.toContain(languageEnglish.risuNest.persistentData.refreshHelp)
    conflict.set(null); await tick()
    expect(document.querySelector('[role="dialog"]')).toBeNull()
})
it('permits a failed action to be chosen again without showing the thrown payload', async () => {
    resolve.mockRejectedValueOnce(new Error('private failure'))
    await show()
    button(copy.saveCopy).click(); await tick(); await tick()
    expect(document.querySelector('[role="alert"]')?.textContent).toBe(copy.failed)
    expect(document.body.textContent).not.toContain('private failure')
    expect(button(copy.saveCopy).disabled).toBe(false)
})
it.each(['failed', 'committed'] as const)('does not apply a stale %s result or discard confirmation to a replacement conflict', async outcome => {
    let fail!: (reason: unknown) => void
    let finish!: (outcome: Outcome) => void
    resolve.mockImplementationOnce(() => new Promise<Outcome>((done, reject) => { finish = done; fail = reject }))
    await show()
    button(copy.discard).click(); await tick()
    button(copy.discard).click(); await tick()
    await show({ id: 'synthetic-next' })
    expect(document.body.textContent).not.toContain(copy.discardConfirm)
    expect(button(copy.saveCopy).disabled).toBe(true)
    if (outcome === 'failed') fail(new Error('old failure'))
    else finish({ kind: 'committed-refresh-required' })
    await tick(); await tick()
    expect(document.querySelector('[role="alert"]')).toBeNull()
    expect(button(copy.saveCopy).disabled).toBe(false)
})

it('allows a fresh choice after conflict-changed without automatically retrying it', async () => {
    resolve.mockResolvedValueOnce({ kind: 'conflict-changed' })
    await show()
    button(copy.saveCopy).click(); await tick(); await tick()
    expect(button(copy.saveCopy).disabled).toBe(false)
    expect(button(copy.discard).disabled).toBe(false)
    expect(document.querySelector('[role="dialog"]')).not.toBeNull()
    expect(document.querySelector('[role="alert"]')).toBeNull()
    expect(resolve).toHaveBeenCalledOnce()
})