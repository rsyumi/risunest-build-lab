import { get } from 'svelte/store'
import { modalNavigation } from '../../src/ts/ui/modalNavigation'
import { alertStore } from './modalNavigationAdapters'

const host = document.querySelector<HTMLElement>('#modals')!
const opener = document.querySelector<HTMLButtonElement>('#opener')!
let parent: ReturnType<typeof createModal> | undefined
let child: ReturnType<typeof createModal> | undefined
const closed = { parent: 0, child: 0 }
let pops = 0
window.addEventListener('popstate', () => { pops++ })
history.replaceState({ page: 'before' }, '', '#before')
history.pushState({ page: 'baseline' }, '', '#baseline')
function createModal(name: 'parent' | 'child') {
    const node = document.createElement('section')
    node.id = name
    const button = document.createElement('button')
    button.id = `${name}-control`
    button.textContent = `${name} control`
    node.append(button)
    host.append(node)
    const action = modalNavigation(node, { close() {
        closed[name]++
        action.destroy()
        node.remove()
    } })
    return { node, action }
}
const modalHistory = {
    async open(nested = false) {
        opener.focus()
        parent = createModal('parent')
        await Promise.resolve()
        if (nested) child = createModal('child')
    },
    destroyNested() {
        child!.action.destroy()
        child!.node.remove()
        parent!.action.destroy()
        parent!.node.remove()
    },
    pushForeign() { history.pushState(null, '', '#foreign') },
    showAlert() {
        alertStore.set({ type: 'normal', msg: 'Synthetic alert' })
        opener.focus()
    },
    state() { return { closed: { ...closed }, pops, hash: location.hash, state: history.state,
        focus: (document.activeElement as HTMLElement | null)?.id,
        parentConnected: parent?.node.isConnected ?? false, alert: get(alertStore).type } },
}
Object.assign(window, { modalHistory })
export type NavigationDriver = typeof modalHistory
