import { writable } from 'svelte/store'
import type { Database } from '../../src/ts/storage/database.svelte'
import { tokenizerStubModule } from '../../src/ts/tokenizer.testSupport'
export { languageEnglish as language } from '../../src/lang/en'

export const DBState = $state({ db: {} as Database })
export const selectedCharID = writable(0)
export const alertConfirm = async () => true
export const tokenizeAccurate = async () => 0
export const tokenizePreset = async () => 0
export const { countTogether } = tokenizerStubModule()
export const templateCheck = () => []
export const getCurrentCharacter = () => DBState.db.characters[0]
export { sortableOptions } from '../../src/ts/sortableOptions'
