import { writable } from 'svelte/store'
export const fixtureState = {
    database: { characters: [], plugins: [] as any[] },
    writes: [] as { key: string; value: unknown }[],
    releaseHeld: null as null | (() => void),
    callbacks: new Set<(...args: any[]) => Promise<unknown>>(),
}
export const allowedDbKeys = []
export const customProviderStore = writable([])
export const pluginV2 = { providers: new Map(), providerOptions: new Map(), chatOutput: new Set() }
export const getV2PluginAPIs = () => ({})
export const pluginStorageStore = { forOwner: () => ({
    setItem: async (key: string, value: unknown) => { fixtureState.writes.push({ key, value }) },
    getItem: async (key: string) => key === 'held'
        ? new Promise(resolve => { fixtureState.releaseHeld = () => resolve('late-old-value') })
        : 'current-value',
}) }
export const getDatabase = () => fixtureState.database
export const DBState = { db: fixtureState.database }
export const selectedCharID = writable(0)
export const doingChat = writable(false)
export const additionalChatMenu: any[] = []
export const additionalFloatingActionButtons: any[] = []
export const additionalHamburgerMenu: any[] = []
export const additionalSettingsMenu: any[] = []
export const bodyIntercepterStore: any[] = []
export const chatPanelStore: any[] = []
export class SafeLocalStorage { constructor(_owner: string) {} }
export class SafeLocalPluginStorage {}
export const tagWhitelist = []
export const beginPluginClaimSession = async () => null
export const sleep = (milliseconds: number) => new Promise(resolve => setTimeout(resolve, milliseconds))
export const language = {}
export const isTauri = false
export const assertPersistentMutationAllowed = () => {}
export const getPluginPermissionStore = () => ({ getItem: async () => Date.now(), setItem: async () => {} })
export const registerTTSPreprocessor = (callback: (...args: any[]) => Promise<unknown>) => fixtureState.callbacks.add(callback)
export const unregisterTTSPreprocessor = (callback: (...args: any[]) => Promise<unknown>) => fixtureState.callbacks.delete(callback)
const unrelated = () => { throw new Error('Unrelated product boundary reached by browser fixture') }
export const acquireCompleteConversation = unrelated
export const alertConfirm = unrelated
export const alertError = unrelated
export const alertNormal = unrelated
export const appendCurrentConversationMessage = unrelated
export const applyPreparedPluginDatabaseUpdate = unrelated
export const captureSelectedConversationTarget = unrelated
export const changeColorScheme = unrelated
export const checkCharOrder = unrelated
export const createProductionPluginDatabaseAccess = unrelated
export const flushPendingDataLocally = unrelated
export const forageStorage = unrelated
export const getActiveConversationSession = unrelated
export const getFetchLogs = unrelated
export const getInlayAsset = unrelated
export const getLLMCache = unrelated
export const getModelInfo = unrelated
export const getModuleLorebooks = unrelated
export const getPersistentNavigationGeneration = unrelated
export const getPersistentStorageAuthorityEpoch = unrelated
export const handlePluginInstallViaPlugin = unrelated
export const hasher = unrelated
export const invalidateActiveConversationSession = unrelated
export const isArchivedCharacter = unrelated
export const linkPluginQueryAbortSignals = unrelated
export const materializePersistentDatabaseSnapshotWithRevision = unrelated
export const refreshSelectedConversationAfterReplacement = unrelated
export const registerMCPModule = unrelated
export const registerTTSPostprocessor = unrelated
export const replacePersistentCompleteCharacter = unrelated
export const replacePersistentConversation = unrelated
export const replacePersistentDatabase = unrelated
export const requestChatDataMain = unrelated
export const reserveGeneration = unrelated
export const searchLLMCache = unrelated
export const sendChat = unrelated
export const unregisterMCPModule = unrelated
export const unregisterTTSPostprocessor = unrelated
export const updateColorScheme = unrelated
export const updateTextThemeAndCSS = unrelated
