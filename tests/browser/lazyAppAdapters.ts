import { get, writable } from 'svelte/store'
import { alertStore } from './modalNavigationAdapters'
import { languageEnglish } from '../../src/lang/en'
import { languageKorean } from '../../src/lang/ko'

export const language = new URLSearchParams(location.search).get('locale') === 'en' ? languageEnglish : languageKorean
export const DBState = { db: { didFirstSetup: true, characters: [], keepSessionAlive: false } }
export const DynamicGUI = writable(false), settingsOpen = writable(false), sideBarStore = writable(true), sideBarClosing = writable(false)
export const ShowRealmFrameStore = writable(false), openPresetList = writable(false), openPersonaList = writable(false)
export const MobileGUI = writable(false), CustomGUISettingMenuStore = writable(false), loadedStore = writable(true)
export { alertStore }
export const bookmarkListOpen = writable(false), bootFailure = writable(null), recoveryStart = writable(null)
export const LoadingStatusState = { startedAt: null, text: '' }
export const popupStore = {}, easyPanelStore = {}, popUpEditorStore = {}, loadoutModalStore = {}, irisStore = {}, customSideBarConfigDialogStore = {}
export const hypaV3ModalOpen = writable(false), hypaV3ProgressStore = writable({ open: false })
export const MobileGUIStack = writable(1), MobileSideBar = writable(0), selectedCharID = writable(-1), CharConfigSubMenu = writable(0)
export const onboardingHold = writable(false), showRealmInfoStore = writable(null), SettingsMenuIndex = writable(0)
export const persistentWorkingSetInputBlocked = writable(false), navigationActivity = writable(null), isLite = writable(false)
export const serverSyncNavigation = writable(null), serverSyncScreenRequest = writable(null), dataHealthNavigation = writable(null)
export const isTauri = false, isTauriMobile = false, DATA_HEALTH_SECTION_ID = 'synthetic-data-health'
export const RECOVERY_EXCLUSIONS = []
export const importCharacterProcess = () => {}, importPreset = () => {}, getDatabase = () => DBState.db, setDatabase = () => {}
export const readModule = () => {}, alertConfirm = () => {}, alertNormal = () => {}, alertToast = () => {}, checkCharOrder = () => {}
export const keepFocusedInputVisible = () => {}, restoreFocusAfterInputBlock = () => {}
export { backNavigationLayer, handleRootBack } from '../../src/ts/ui/modalNavigation'
export const offerToKeepRecoveryExclusions = async () => false, isStartupExcluded = () => true, getStartupExclusions = () => [], updateStartupExclusions = () => {}
export const exportOriginalData = () => {}, openRisuNestSettingsTab = () => {}
export const textEditorPopup = { request: null }

let failingRoute = ''
let attempts = 0
let holdFirst = false
let rejectHeld: (() => void) | undefined
export const lazyImportBoundary = {
    arm(route: string) { failingRoute = route === 'mobile' ? 'settings' : route; attempts = 0; holdFirst = false },
    hold(route: string) { this.arm(route); holdFirst = true },
    rejectHeld() { rejectHeld!() },
    attempts() { return attempts },
    async load(route: string, load: () => Promise<unknown>) {
        if (route === failingRoute && ++attempts === 1) {
            if (!holdFirst) throw new Error('Synthetic one-shot import rejection')
            await new Promise<void>((_resolve, reject) => { rejectHeld = () => reject(new Error('Synthetic stale import rejection')) })
        }
        return load()
    },
}

export function openRoute(route: string) {
    if (route === 'settings') settingsOpen.set(true)
    else if (route === 'custom') CustomGUISettingMenuStore.set(true)
    else if (route === 'presets') openPresetList.set(true)
    else if (route === 'personas') openPersonaList.set(true)
    else if (route === 'mobile') { MobileGUI.set(true); MobileGUIStack.set(2) }
    else if (route === 'sidebar') { DynamicGUI.set(true); sideBarStore.set(true) }
    else throw new Error(`Unknown synthetic route: ${route}`)
}

export const navigationState = {
    showDialog() { alertStore.set({ type: 'ask', msg: 'Synthetic confirmation' }) },
    read() { return { sidebarClosing: get(sideBarClosing), dialog: get(alertStore).type, entries: history.state?.risunestModal?.length ?? 0 } },
}
