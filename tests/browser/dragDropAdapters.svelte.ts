import { writable } from 'svelte/store'
import type { Database } from '../../src/ts/storage/database.svelte'
export { languageEnglish as language } from '../../src/lang/en'

export const DBState = $state({ db: {} as Database })
export const selectedCharID = writable(0)
export const ReloadGUIPointer = writable(0)
export const alertConfirm = async () => true
export const alertCheckboxConfirm = async () => ({ confirmed: false, checked: false })
export const alertError = (message: string) => { throw new Error(message) }
export const alertMd = () => {}
export const tokenizeAccurate = async () => 0
export const tokenizePreset = async () => 0
export const templateCheck = () => []
export const exportRegex = () => {}
export const importRegex = async (value: unknown[]) => value
export const getCurrentCharacter = () => DBState.db.characters[0]
export const getCurrentChat = () => DBState.db.characters[0].chats[0]
export const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))
export const sortableOptions = {
    delay: 300, delayOnTouchOnly: true, filter: '.no-sort',
    onMove: (event: { related: HTMLElement }) => !event.related.className.includes('no-sort'),
}
