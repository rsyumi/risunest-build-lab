import { get } from 'svelte/store'
import { language } from 'src/lang'
import { DBState, selIdState, settingsOpen } from '../stores.svelte'
import { doingChat } from '../process/generationState'
import { isArchivedCharacter } from '../storage/workingSetCatalog'
import { getPersistentDataStore } from '../storage/persistentDataStoreFactory'
import { captureSelectedConversationTarget } from '../storage/persistentDataRuntime.svelte'
import { alertToast } from '../alert'
import { galleryText } from './strings'
import { galleryOriginIds } from './inlayGalleryState'
import type { GalleryItem } from './types'
import type { GalleryLocation } from './inlayGallery'

export interface GalleryOrigin extends GalleryLocation {
    characterId: string
    characterName: string
    conversationId: string
    conversationName?: string
}
export async function readGalleryOrigin(item: GalleryItem): Promise<GalleryOrigin | null> {
    const ids = galleryOriginIds(item.metadata)
    if (!ids) return null
    const { characterId, conversationId } = ids
    const character = DBState.db.characters.find(value => value.chaId === characterId)
    if (!character) return null
    const base: GalleryOrigin = { kind: 'conversation', characterId, conversationId, characterName: character.name,
        archived: isArchivedCharacter(character), trashed: !!character.trashTime }
    if (base.archived) return base
    const conversation = await getPersistentDataStore().readConversationMetadata(characterId, conversationId)
    return conversation ? { ...base, conversationName: conversation.value.conversation.name } : null
}
export async function jumpToGalleryLocation(location: GalleryLocation, closeViewer: () => void, origin = false): Promise<boolean> {
    if (get(doingChat)) { alertToast(language.navigationBlockedWhileGenerating); return false }
    const strings = galleryText()
    const missing = origin ? strings.conversationMissing : strings.messageMissing
    const index = DBState.db.characters.findIndex(value => value.chaId === location.characterId)
    const character = DBState.db.characters[index]
    if (!character || character.trashTime || isArchivedCharacter(character) || !location.conversationId) {
        alertToast(missing); return false
    }
    const [{ changeChar }, { changeChatTo, foldChatToMessage }] = await Promise.all([import('../characters'), import('../globalApi.svelte')])
    // Imports can yield while generation starts, including on the current target.
    if (get(doingChat)) { alertToast(language.navigationBlockedWhileGenerating); return false }
    closeViewer()
    settingsOpen.set(false)
    if (!await changeChar(index)) return false
    if (DBState.db.characters[selIdState.selId]?.chaId !== location.characterId) { alertToast(missing); return false }
    if (!await changeChatTo(location.conversationId)) { alertToast(missing); return false }
    const selected = captureSelectedConversationTarget()
    if (selected?.characterId !== location.characterId || selected.conversationId !== location.conversationId) {
        alertToast(missing); return false
    }
    if (!origin) {
        const target = location.messageId ?? location.messageIndex
        if (target === undefined || !await foldChatToMessage(target)) { alertToast(missing); return false }
    }
    return true
}
