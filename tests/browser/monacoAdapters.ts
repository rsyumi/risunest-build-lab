import { writable } from 'svelte/store'

export const ColorSchemeTypeStore = writable('dark')
export const language = { risuNest: { textEditor: { save: 'Save' } } }
export function registerCBSMonaco() {}
