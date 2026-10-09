import { writable } from 'svelte/store'

// The fixture has no character scripts, assets, translation, or persistent data.
// Parser, sanitizer, Markdown libraries, ChatBody and preview rendering stay real.
export const DBState = { db: {
    characters: [], autoTranslate: false, hideAllImages: true,
    unformatQuotes: true, newImageHandlingBeta: false,
} }
export const selIdState = { selId: -1 }
export const selectedCharID = writable(-1)
export const language = { cot: 'Synthetic thought', toolCalled: '{{tool}}' }
export const appVer = 'synthetic'
export const isTauri = false
export const getDatabase = () => DBState.db
export const getCurrentCharacter = () => null
export const findCharacterbyId = () => null
export const aiWatermarkingLawApplies = () => false
export const getModuleAssets = () => []
export const getModuleLorebooks = () => []
export const getModules = () => []
export const getUserName = () => 'Synthetic user'
export const getPersonaPrompt = () => ''
export const getUserIcon = () => ''
export const getChatVar = () => 'null'
export const getGlobalChatVar = () => 'null'
const unexpected = () => { throw new Error('Unexpected settled-thought fixture service access') }
export const getFileImageSource = unexpected
export const getFileSrc = unexpected
export const setChatVar = unexpected
export const processScriptFull = unexpected
export const calcString = unexpected
export const pickHashRand = unexpected
export const replaceAsync = unexpected
export const getInlayAssetMetadata = unexpected
export const getInlayRenderSources = unexpected
export const renderDeferredInlaySourceMarkup = unexpected
export const getModelInfo = unexpected
export const registerCBS = unexpected
export const getLLMCache = unexpected
export const translateHTML = unexpected
export const alertError = unexpected
export const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))
export class DeferredInlayMarkerRegistry { clear() {} }
export const mountDeferredInlaySources = () => () => {}
export const resolveDeferredInlaySources = async () => () => {}
