import { mount } from 'svelte'
import App from '../../src/App.svelte'
import { openRoute, lazyImportBoundary, navigationState } from './lazyAppAdapters'

Object.assign(window, { lazyImportBoundary, lazyApp: { open: openRoute, ...navigationState } })
mount(App, { target: document.querySelector<HTMLElement>('#app')! })
