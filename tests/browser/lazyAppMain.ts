import { mount } from 'svelte'
import App from '../../src/App.svelte'
import { openRoute, lazyImportBoundary } from './lazyAppAdapters'

Object.assign(window, { lazyImportBoundary, lazyApp: { open: openRoute } })
mount(App, { target: document.querySelector<HTMLElement>('#app')! })
