import { writable } from 'svelte/store'
export const alertStore = writable({ type: 'none', msg: '' })
