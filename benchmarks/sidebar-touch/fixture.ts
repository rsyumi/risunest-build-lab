import { sidebarTouchDrag } from '../../src/ts/gui/sidebarTouchDrag'
import { RISU_SIDEBAR_DRAG_TYPE } from '../../src/ts/dragTypes'
import { polyfill } from 'mobile-drag-drop'

export function mountSidebarTouchFixture(host: HTMLElement) {
    // Exercise coexistence with the legacy iOS document-level polyfill.
    polyfill({ forceApply: true })
    host.style.cssText = 'font:16px sans-serif;color:black;background:white;padding:8px'
    host.innerHTML = '<button id="reset-sidebar" style="padding:12px">Reset sidebar</button><div id="sidebar-status"></div><div id="touch-sidebar" style="width:100px;height:360px;overflow-y:auto;user-select:none;border:1px solid gray"></div>'
    const list = host.querySelector<HTMLElement>('#touch-sidebar')!
    const status = host.querySelector<HTMLElement>('#sidebar-status')!
    let order: number[] = []
    let source = -1
    let drops = 0
    let clicks = 0
    let menus = 0
    let action: ReturnType<typeof sidebarTouchDrag> | undefined
    function update() {
        status.innerHTML = `<p>sidebar-drops:${drops}</p><p>sidebar-clicks:${clicks}</p><p>sidebar-menus:${menus}</p><p>sidebar-first:${order[0]}</p><p>${list.scrollTop > 10 ? 'sidebar-scrolled' : 'sidebar-top'}</p>`
    }
    function render() {
        list.replaceChildren()
        for (const id of order) {
            const gap = document.createElement('div')
            gap.dataset.sidebarDrop = ''
            gap.setAttribute('role', 'button')
            gap.setAttribute('aria-label', `Before Bot ${id}`)
            gap.style.cssText = 'height:20px;background:#eee'
            gap.addEventListener('dragover', event => {
                if (event.dataTransfer!.types.includes(RISU_SIDEBAR_DRAG_TYPE)) event.preventDefault()
            })
            gap.addEventListener('drop', event => {
                event.preventDefault()
                order = order.filter(value => value !== source)
                order.splice(order.indexOf(id), 0, source)
                drops++
                queueMicrotask(() => { render(); update() })
            })
            const row = document.createElement('div')
            row.dataset.sidebarDragId = String(id)
            row.draggable = true
            row.innerHTML = `<span class="avatar" role="button" aria-label="Bot ${id}" tabindex="0" style="display:flex;align-items:center;width:80px;height:56px;background:#bcd"><img alt="" width="80" height="56" src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='80' height='56'%3E%3Crect width='80' height='56' fill='%23bcd'/%3E%3C/svg%3E"></span>`
            row.addEventListener('dragstart', event => {
                source = id
                event.dataTransfer!.setData(RISU_SIDEBAR_DRAG_TYPE, 'true')
            })
            row.addEventListener('click', () => { clicks++; update() })
            row.addEventListener('contextmenu', event => { event.preventDefault(); menus++; update() })
            list.append(gap, row)
        }
    }
    function reset() {
        action?.destroy()
        order = Array.from({ length: 20 }, (_, i) => i + 1)
        drops = clicks = menus = 0
        render()
        list.scrollTop = 0
        action = sidebarTouchDrag(list)
        update()
    }
    list.addEventListener('scroll', update)
    host.querySelector('#reset-sidebar')!.addEventListener('click', reset)
    reset()
}
