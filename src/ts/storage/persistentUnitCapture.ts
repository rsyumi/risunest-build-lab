import type { Chat, character, groupChat } from './database.svelte'
import type { ConversationMutation, PersistentUnitMutation } from './persistentDataStore'
import { canonicalJson, canonicalClone, messageReplaceRange } from './saveCoordinatorHelpers'
import { isConversationSummaryStub } from './conversationResidency'

type CompleteCharacter = character | groupChat

export function diffFields(
    components: string[], beforeValue: object, afterValue: object,
    excluded: ReadonlySet<string> = new Set(),
): PersistentUnitMutation[] {
    const before = beforeValue as Record<string, unknown>, after = afterValue as Record<string, unknown>
    const mutations: PersistentUnitMutation[] = []
    for (const field of new Set([...Object.keys(before), ...Object.keys(after)])) {
        if (excluded.has(field) || before[field] === after[field]) continue
        if (canonicalJson({ value: before[field] }) === canonicalJson({ value: after[field] })) continue
        const key = JSON.stringify([...components, field])
        mutations.push(!Object.hasOwn(after, field) || after[field] === undefined
            ? { key, type: 'delete' } : { key, type: 'set', value: canonicalClone(after[field]) })
    }
    return mutations
}

export function captureMaterializedCharacter(value: CompleteCharacter): CompleteCharacter {
    const result = Object.fromEntries(Object.keys(value).filter((key) => key !== 'chats' && value[key] !== undefined)
        .map((key) => [key, canonicalClone(value[key])])) as CompleteCharacter
    result.chats = value.chats.map((chat) => Object.fromEntries(Object.keys(chat)
        .filter((key) => chat[key] !== undefined && (key !== 'message' || (!isConversationSummaryStub(chat)
            && Object.prototype.propertyIsEnumerable.call(chat, 'message'))))
        .map((key) => [key, canonicalClone(chat[key])])) as Chat)
    return result
}

export function diffMaterializedCharacter(before: CompleteCharacter, after: CompleteCharacter): {
    unitMutations: PersistentUnitMutation[]; conversations: ConversationMutation[]
} {
    const unitMutations = diffFields(['character', after.chaId], before, after,
        new Set(['chats', 'chaId', 'characters', 'characterTalks', 'characterActive', 'chatFolders']))
    if (after.type === 'group' && canonicalJson([(before as groupChat).characters, (before as groupChat).characterTalks, (before as groupChat).characterActive])
        !== canonicalJson([after.characters, after.characterTalks, after.characterActive])) {
        unitMutations.push({ key: JSON.stringify(['group-members', after.chaId]), type: 'set',
            value: { characters: after.characters, characterTalks: after.characterTalks, characterActive: after.characterActive } })
    }
    const conversations: ConversationMutation[] = []
    const previous = new Map(before.chats.map((chat) => [chat.id, chat]))
    for (const chat of after.chats) {
        if (!chat.id) throw new TypeError('Conversation requires a stable ID')
        const old = previous.get(chat.id)
        if (!old) {
            if (!Object.hasOwn(chat, 'message')) throw new TypeError('New conversation requires messages')
            const { message, ...conversation } = chat
            conversations.push({ type: 'replace-range', characterId: after.chaId, conversationId: chat.id,
                start: 0, deleteCount: 0, messages: message, conversation })
            continue
        }
        unitMutations.push(...diffFields(['conversation', after.chaId, chat.id], old, chat, new Set(['message', 'id'])))
        if (chat.message !== old.message && Object.hasOwn(chat, 'message') && Object.hasOwn(old, 'message')
            && canonicalJson(chat.message) !== canonicalJson(old.message)) {
            conversations.push({ type: 'replace-range', characterId: after.chaId, conversationId: chat.id,
                ...messageReplaceRange(old.message, chat.message) })
        }
    }
    const nextIds = new Set(after.chats.map((chat) => chat.id))
    for (const old of before.chats) if (!nextIds.has(old.id)) {
        conversations.push({ type: 'delete', characterId: after.chaId, conversationId: old.id })
    }
    const orderValue = (value: CompleteCharacter) => ({ ids: value.chats.map((chat) => chat.id), folders: value.chatFolders ?? [] })
    if (canonicalJson(orderValue(before)) !== canonicalJson(orderValue(after))) {
        unitMutations.push({ key: JSON.stringify(['order', 'conversations', after.chaId]), type: 'set', value: orderValue(after) })
    }
    return { unitMutations, conversations }
}

export const recordCollections = new Map([
    ['modules', 'id'], ['plugins', 'name'], ['loadouts', 'id'], ['customModels', 'id'],
])

export function diffRecordCollection(collection: string, before: unknown[], after: unknown[]): PersistentUnitMutation[] {
    const identity = recordCollections.get(collection)
    if (!identity) throw new TypeError('Unknown record collection')
    const index = (values: unknown[]) => new Map(values.map((value) => {
        const id = (value as Record<string, unknown>)[identity]
        if (typeof id !== 'string' || !id) throw new TypeError(`${collection} requires stable IDs`)
        return [id, value] as const
    }))
    const previous = index(before), next = index(after)
    if (previous.size !== before.length || next.size !== after.length) throw new TypeError('Duplicate record ID')
    const mutations: PersistentUnitMutation[] = []
    for (const id of new Set([...previous.keys(), ...next.keys()])) {
        if (canonicalJson(previous.get(id) ?? null) === canonicalJson(next.get(id) ?? null)) continue
        if (collection !== 'plugins' && previous.has(id) !== next.has(id)) mutations.push(next.has(id)
            ? { key: JSON.stringify(['exists', collection, id]), type: 'set', value: true }
            : { key: JSON.stringify(['exists', collection, id]), type: 'delete' })
        mutations.push(next.has(id) ? { key: JSON.stringify(['record', collection, id]), type: 'set', value: next.get(id) }
            : { key: JSON.stringify(['record', collection, id]), type: 'delete' })
    }
    if (canonicalJson([...previous.keys()]) !== canonicalJson([...next.keys()])) {
        mutations.push({ key: JSON.stringify(['order', collection]), type: 'set', value: [...next.keys()] })
    }
    return mutations
}

export const CHARACTER_SHARED_FIELDS: ReadonlySet<string> = new Set(['statics', 'additionalAssets', 'additionalData', 'additionalText', 'alternateGreetings', 'autoMode', 'backgroundCSS', 'backgroundHTML', 'bias', 'ccAssets', 'characterVersion', 'coldStoragedChats', 'coldstorage', 'creation_date', 'creator', 'creatorNotes', 'customModuleToggle', 'customscript', 'defaultVariables', 'depth_prompt', 'desc', 'doNotChangeSeperateModels', 'emotionImages', 'escapeOutput', 'exampleMessage', 'extentions', 'firstMessage', 'firstMsgIndex', 'fishSpeechConfig', 'globalLore', 'gptSoVitsConfig', 'group_only_greetings', 'hfTTS', 'hideChatIcon', 'image', 'imported', 'inlayViewScreen', 'largePortrait', 'license', 'loreExt', 'lorePlus', 'loreSettings', 'lowLevelAccess', 'modification_date', 'moduleNamespace', 'modules', 'naittsConfig', 'name', 'newGenData', 'nickname', 'notes', 'oaiTTSConfig', 'oaiVoice', 'oneAtTime', 'orderByOrder', 'personality', 'postHistoryInstructions', 'prebuiltAssetCommand', 'prebuiltAssetExclude', 'prebuiltAssetStyle', 'private', 'realmId', 'removedQuotes', 'replaceGlobalNote', 'scenario', 'scriptstate', 'sdData', 'source', 'suggestMessages', 'supaMemory', 'systemPrompt', 'tags', 'translatorNote', 'trashTime', 'triggerscript', 'ttsMode', 'ttsReadOnlyQuoted', 'ttsSpeech', 'useCharacterLore', 'utilityBot', 'viewScreen', 'virtualscript', 'vits', 'voicevoxConfig'])
export const CONVERSATION_SHARED_FIELDS: ReadonlySet<string> = new Set(['GLGlobalVariables', 'bindedPersona', 'bookmarkNames', 'bookmarks', 'fmIndex', 'folderId', 'hypaV2Data', 'hypaV3Data', 'lastDate', 'lastMemory', 'localLore', 'modules', 'name', 'note', 'rerollRecovery', 'savedToggleValues', 'scriptstate', 'sdData', 'suggestMessages', 'supaMemoryData', 'toolCalls', 'useLocallySetGlobalVariables'])
