import { mountSidebarTouchFixture } from '../../benchmarks/sidebar-touch/fixture'
if (new URLSearchParams(location.search).has('product')) {
    const { mount } = await import('svelte')
    const { default: Sidebar } = await import('../../src/lib/SideBars/Sidebar.svelte')
    const { DBState } = await import('./sidebarProductAdapters.svelte')
    const { polyfill } = await import('mobile-drag-drop')
    polyfill({ forceApply: true })
    Object.assign(window, { sidebarModel: DBState })
    document.body.style.cssText = 'display:flex;height:640px;margin:0'
    mount(Sidebar, { target: document.body })
} else {
    mountSidebarTouchFixture(document.body)
}
