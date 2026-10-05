import type { PluginFieldIntent, PluginReadKind } from './pluginReadBaselines'

import type { PersistentUnitMutation, WholeMessageIntent } from '../storage/persistentDataStore'
import type { Message } from '../storage/database.svelte'

export type PluginUnitMutation = PersistentUnitMutation
export type PluginWholeMessageIntent = WholeMessageIntent

export function pluginUnitIntents(
    intents: readonly PluginFieldIntent[],
    kind: PluginReadKind,
    owner: string,
    submitted: any,
    characterId?: string,
    conversationId?: string,
): { units: PluginUnitMutation[]; wholeMessages: PluginWholeMessageIntent[] } {
    const units: PluginUnitMutation[] = []
    const wholeMessages: PluginWholeMessageIntent[] = []
    const emit = (parts: string[], intent: PluginFieldIntent) => {
        const key = JSON.stringify(parts)
        units.push(intent.type === 'delete' || intent.value === undefined ? { key, type: 'delete' } : { key, type: 'set', value: intent.value })
    }
    const conversation = (id: string, chatId: string, fields: Array<string | number>, intent: PluginFieldIntent) => {
        if (!fields.length) {
            if (intent.type === 'delete') emit(['exists', 'conversation', id, chatId], intent)
            else {
                emit(['exists', 'conversation', id, chatId], { ...intent, value: true })
                for (const [field, value] of Object.entries(intent.value as object)) {
                    if (field !== 'id') conversation(id, chatId, [field], { ...intent, value })
                }
            }
        } else if (fields[0] === 'message') {
            wholeMessages.push({ characterId: id, conversationId: chatId, messages: intent.type === 'delete' ? [] : intent.value as Message[] })
        } else if (fields[0] !== 'id') emit(['conversation', id, chatId, String(fields[0])], intent)
    }
    const character = (id: string, fields: Array<string | number>, intent: PluginFieldIntent) => {
        if (!fields.length) {
            if (intent.type === 'delete') emit(['exists', 'character', id], intent)
            else {
                const type = (intent.value as any).type
                if (type !== 'character') throw new TypeError('New plugin character requires a structural type')
                emit(['exists', 'character', id], { ...intent, value: { type } })
                for (const [field, value] of Object.entries(intent.value as object)) {
                    if (field === 'chats') {
                        for (const chat of value as any[]) conversation(id, chat.id, [], { ...intent, value: chat })
                        emitConversationOrder(id, intent)
                    } else character(id, [field], { ...intent, value })
                }
            }
        } else if (fields[0] === 'chats') {
            if (fields[1] === '$order') emitConversationOrder(id, intent)
            else conversation(id, String(fields[1]), fields.slice(2), intent)
        } else if (fields[0] === 'chatFolders') {
            emitConversationOrder(id, intent)
        } else if (!['chaId', 'type'].includes(String(fields[0]))) emit(['character', id, String(fields[0])], intent)
    }
    const submittedCharacter = (id: string) => kind === 'character' ? submitted : submitted.characters?.find((record: any) => record.chaId === id)
    const emitConversationOrder = (id: string, intent: PluginFieldIntent) => {
        const record = submittedCharacter(id)
        emit(['order', 'conversations', id], { ...intent, type: 'set', value: { ids: record.chats.map((chat: any) => chat.id), folders: record.chatFolders ?? [] } })
    }
    for (const intent of intents) {
        if (kind === 'character') { character(characterId!, intent.path, intent); continue }
        if (kind === 'conversation') { conversation(characterId!, conversationId!, intent.path, intent); continue }
        const [field, id, ...fields] = intent.path.map(String)
        if (field === 'characters') {
            if (id === '$order') emit(['order', 'characters'], intent)
            else character(id, fields, intent)
        } else if (field === 'botPresets' || field === 'personas') {
            const recordKind = field === 'botPresets' ? 'preset' : 'persona'
            if (id === '$order') emit(['order', recordKind === 'preset' ? 'presets' : 'personas'], intent)
            else if (!fields.length) {
                emit(['exists', recordKind, id], intent.type === 'delete' ? intent : { ...intent, value: true })
                if (intent.type === 'set') for (const [key, value] of Object.entries(intent.value as object)) if (key !== 'id') emit([recordKind, id, key], { ...intent, value })
            } else if (fields[0] !== 'id') emit([recordKind, id, fields[0]], intent)
        } else if (['modules', 'loadouts', 'customModels', 'plugins'].includes(field)) {
            if (id === '$order') emit(['order', field], intent)
            else {
                if (field !== 'plugins') emit(['exists', field, id], intent.type === 'delete' ? intent : { ...intent, value: true })
                emit(['record', field, id], intent)
            }
        } else if (field === 'pluginCustomStorage') emit(['plugin', owner, id], intent)
        else if (field === 'globalChatVariables') emit([id.startsWith('toggle_') ? 'toggle' : 'variable', id], intent)
        else if (field === 'characterOrder') emit(['order', 'characters'], intent)
        else emit(['root', field], intent)
    }
    return { units: [...new Map(units.map(unit => [unit.key, unit])).values()], wholeMessages }
}
