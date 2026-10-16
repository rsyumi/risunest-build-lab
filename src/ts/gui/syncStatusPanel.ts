import { writable } from 'svelte/store'

/** The open status panel: `side` opens beside the sidebar column, `below` under the mobile header. */
export const syncStatusPanel = writable<{ anchor: HTMLElement; placement: 'side' | 'below' } | null>(null)
