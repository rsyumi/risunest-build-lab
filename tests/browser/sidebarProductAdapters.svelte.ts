import { writable } from 'svelte/store'
import type { Database } from '../../src/ts/storage/database.svelte'
export { languageEnglish as language } from '../../src/lang/en'

export const DBState = $state({ db: {
    characters: Array.from({ length: 20 }, (_, i) => ({ chaId: `bot-${i + 1}`, name: `Bot ${i + 1}`, chats: [] })),
    characterOrder: Array.from({ length: 20 }, (_, i) => `bot-${i + 1}`),
    showFolderName: true,
} as unknown as Database, clicks: 0, menus: 0 })
export const selectedCharID = writable(-1)
export const CharEmotion = writable({})
export const DynamicGUI = writable(false)
export const botMakerMode = writable(false)
export const settingsOpen = writable(false)
export const sideBarClosing = writable(false)
export const sideBarStore = writable(0)
export const OpenRealmStore = writable(false)
export const PlaygroundStore = writable(0)
export const QuickSettings = writable(false)
export const sideBarSize = writable(0)
export const additionalHamburgerMenu = []
export const tooltipRight = () => ({})
export const setDatabase = (db: Database) => { DBState.db = db }
export const checkCharOrder = () => {}
export const getCharacterIndexObject = () => Object.fromEntries(DBState.db.characters.map((c, i) => [c.chaId, i]))
export const changeChar = (index: number) => { DBState.clicks++; selectedCharID.set(index) }
export const clearCharacterSelection = async () => { selectedCharID.set(-1); return true }
export const getCharImage = () => ''
export const getFileSrc = () => ''
export const addCharacter = () => {}
export const selectSingleFile = async () => null
export const saveAsset = async () => ''
export const alertSelect = async () => { DBState.menus++; return '-1' }
export const alertInput = async () => ''
