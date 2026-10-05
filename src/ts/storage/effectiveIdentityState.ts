import type { Database, botPreset } from './database.svelte'
import { safeStructuredClone } from '../polyfill'
import { v4 } from 'uuid'
import { canonicalJson } from './saveCoordinatorHelpers'
import { defineOwnEnumerableProperty } from './ownEnumerableProperty'
import { isConversationSummaryStub } from './conversationResidency'

export const presetMirrorMap = Object.fromEntries([
    ...('apiType localNetworkMode localNetworkTimeoutSec mainPrompt jailbreak globalNote temperature maxContext maxResponse frequencyPenalty PresensePenalty formatingOrder aiModel subModel currentPluginProvider textgenWebUIStreamURL textgenWebUIBlockingURL forceReplaceUrl promptPreprocess bias koboldURL proxyKey ooba ainconfig proxyRequestModel openrouterRequestModel promptTemplate NAIadventure NAIappendName localStopStrings autoSuggestPrompt autoSuggestPrefix autoSuggestClean customProxyRequestModel reverseProxyOobaArgs top_p promptSettings repetition_penalty min_p top_a openrouterProvider useInstructPrompt customPromptTemplateToggle templateDefaultVariables moduleIntergration top_k instructChatTemplate JinjaTemplate jsonSchemaEnabled jsonSchema strictJsonSchema extractJson namedMessageRole messageNameTemplate seperateParametersEnabled seperateParameters customAPIFormat systemContentReplacement systemRoleReplacement customFlags enableCustomFlags thinkingTokens thinkingType deepseekThinkingType adaptiveThinkingEffort deepseekReasoningEffort outputImageModal seperateModelsForAxModels seperateModels modelTools fallbackModels fallbackWhenBlankResponse verbosity dynamicOutput').split(' ').map(key => [key, key]),
    ['NAIsettings', 'NAISettings'], ['presetRegex', 'regex'], ['reasoningEffort', 'reasonEffort'],
]) as Record<string, string>

export const protectedPresetGroups = {
    seperateModelsForAxModels: 'doNotChangeSeperateModels',
    seperateModels: 'doNotChangeSeperateModels',
    fallbackModels: 'doNotChangeFallbackModels',
    fallbackWhenBlankResponse: 'doNotChangeFallbackModels',
    seperateParameters: 'disableSeperateParameterChangeOnPresetChange',
} as const
export type ProtectedPresetField = keyof typeof protectedPresetGroups

type Snapshot = Record<string, unknown>
type Conversation = Database['characters'][number]['chats'][number]
interface ToggleBinding { characterId: string; conversationId: string }
interface IdentityState {
    override: string | null
    presetId: string | undefined
    personaId: string | undefined
    preset: Snapshot
    persona: Snapshot
    flags: Snapshot
    applied: string | undefined
    variables: Record<string, string>
    binding: ToggleBinding | undefined
}
const states = new WeakMap<Database, IdentityState>()
export const personaMirrorMap = { username: 'name', userIcon: 'icon', personaPrompt: 'personaPrompt', userNote: 'note' }
const clone = <T>(value: T): T => value === undefined ? value : safeStructuredClone(value)
const root = (db: Database): Snapshot => db as unknown as Snapshot
const snapshot = (db: Database, map: Record<string, string>): Snapshot =>
    Object.fromEntries(Object.keys(map).map(key => [key, clone(root(db)[key])]))
const equal = (left: unknown, right: unknown): boolean => left === undefined || right === undefined
    ? left === right : canonicalJson(left) === canonicalJson(right)

function state(db: Database): IdentityState {
    let current = states.get(db)
    if (!current) {
        current = {
            override: null, presetId: db.botPresets?.[db.botPresetsId]?.id,
            personaId: db.personas?.[db.selectedPersona]?.id,
            preset: snapshot(db, presetMirrorMap), persona: snapshot(db, personaMirrorMap),
            flags: Object.fromEntries(Object.values(protectedPresetGroups).map(key => [key, root(db)[key]])),
            applied: undefined, variables: { ...db.globalChatVariables }, binding: undefined,
        }
        states.set(db, current)
    }
    return current
}

function boundConversation(db: Database, binding: ToggleBinding | undefined): Conversation | undefined {
    if (!binding) return undefined
    const chat = db.characters?.find(character => character.chaId === binding.characterId)?.chats
        ?.find(chat => chat.id === binding.conversationId)
    return chat && !isConversationSummaryStub(chat) ? chat : undefined
}

export function getEffectivePresetOverride(db: Database): string | null {
    return states.get(db)?.override ?? null
}

export function getEffectivePresetId(db: Database): string | undefined {
    return state(db).override ?? db.botPresets?.[db.botPresetsId]?.id
}

export function flushEffectivePresetEdits(db: Database): void {
    const current = state(db)
    const preset = current.presetId ? db.botPresets?.find(preset => preset.id === current.presetId) : undefined
    if (!preset) return
    const record = preset as unknown as Snapshot
    for (const [key, field] of Object.entries(presetMirrorMap)) {
        const value = root(db)[key]
        const flag = protectedPresetGroups[key as ProtectedPresetField]
        if (flag) {
            const protectedNow = Boolean(root(db)[flag])
            const protectedBefore = Boolean(current.flags[flag])
            if (protectedNow) {
                if (!protectedBefore || !equal(value, current.preset[key])) {
                    db.protectedPresetValues ??= {}
                    db.protectedPresetValues[key as ProtectedPresetField] = clone(value)
                }
                continue
            }
            if (protectedBefore) {
                record[field] = clone(value)
                continue
            }
        }
        if (!equal(value, current.preset[key])) {
            if (value === undefined) delete record[field]
            else record[field] = clone(value)
        }
    }
    current.preset = snapshot(db, presetMirrorMap)
    current.flags = Object.fromEntries(Object.values(protectedPresetGroups).map(key => [key, root(db)[key]]))
}

export function deriveEffectivePresetMirrors(db: Database, apply: (db: Database, preset: botPreset) => void): void {
    const current = state(db)
    if (current.override && !db.botPresets?.some(preset => preset.id === current.override)) current.override = null
    const id = getEffectivePresetId(db)
    const preset = id ? db.botPresets?.find(preset => preset.id === id) : undefined
    const flags = Object.fromEntries(Object.values(protectedPresetGroups).map(key => [key, root(db)[key]]))
    const applied = canonicalJson({ id: id ?? null, preset: preset ?? null, flags, protected: db.protectedPresetValues ?? null })
    // The same inputs over unedited mirrors would rewrite every field with equal copies.
    if (applied === current.applied && Object.keys(presetMirrorMap).every(key => equal(root(db)[key], current.preset[key]))) {
        current.presetId = id
        current.flags = flags
        return
    }
    if (preset) apply(db, preset)
    for (const [key, flag] of Object.entries(protectedPresetGroups)) {
        const value = db.protectedPresetValues?.[key as ProtectedPresetField]
        if (root(db)[flag] && Object.hasOwn(db.protectedPresetValues ?? {}, key) && !equal(root(db)[key], value)) {
            root(db)[key] = clone(value)
        }
    }
    current.presetId = id
    current.applied = applied
    current.preset = snapshot(db, presetMirrorMap)
    current.flags = flags
}

export function setEffectivePresetOverride(db: Database, id: string | null, apply: (db: Database, preset: botPreset) => void): void {
    if (id !== null && !db.botPresets?.some(preset => preset.id === id)) throw new Error('Preset was not found')
    flushEffectivePresetEdits(db)
    state(db).override = id
    deriveEffectivePresetMirrors(db, apply)
}

export function adoptEffectivePresetSelection(db: Database, apply: (db: Database, preset: botPreset) => void): void {
    state(db).override = null
    deriveEffectivePresetMirrors(db, apply)
}

export function flushEffectivePersonaEdits(db: Database): void {
    const current = state(db)
    const persona = current.personaId ? db.personas?.find(persona => persona.id === current.personaId) : undefined
    if (!persona) return
    const record = persona as unknown as Snapshot
    for (const [key, field] of Object.entries(personaMirrorMap)) {
        if (!equal(root(db)[key], current.persona[key])) record[field] = clone(root(db)[key])
    }
    current.persona = snapshot(db, personaMirrorMap)
}

export function deriveEffectivePersonaMirrors(db: Database): void {
    const current = state(db)
    const persona = db.personas?.[db.selectedPersona]
    if (persona) {
        for (const [key, field] of Object.entries(personaMirrorMap)) {
            const value = (persona as unknown as Snapshot)[field]
            if (!equal(root(db)[key], value)) root(db)[key] = clone(value)
        }
    }
    current.personaId = persona?.id
    current.persona = snapshot(db, personaMirrorMap)
}

export function getExplicitGlobalChatVariables(db: Database): Record<string, string> {
    db.explicitGlobalChatVariables ??= { ...db.globalChatVariables }
    return db.explicitGlobalChatVariables
}

export function flushEffectiveToggleEdits(db: Database): void {
    const current = state(db)
    const explicit = getExplicitGlobalChatVariables(db)
    for (const key of new Set([...Object.keys(current.variables), ...Object.keys(db.globalChatVariables ?? {})])) {
        const value = db.globalChatVariables?.[key]
        if (value === current.variables[key]) continue
        const bound = key.startsWith('toggle_') && current.binding ? boundConversation(db, current.binding) : undefined
        // Toggle edits for a bound conversation that is no longer resident never reach shared variables.
        if (key.startsWith('toggle_') && current.binding && !bound) continue
        const target = bound ? (bound.savedToggleValues ??= {}) : explicit
        if (value === undefined) delete target[key]
        else defineOwnEnumerableProperty(target, key, value)
    }
    current.variables = { ...db.globalChatVariables }
}

export function deriveEffectiveToggleVariables(db: Database, chat?: Conversation): void {
    const current = state(db)
    const explicit = getExplicitGlobalChatVariables(db)
    const variables = { ...explicit }
    const owner = !db.disableToggleBinding && chat?.savedToggleValues !== undefined && typeof chat.id === 'string'
        ? db.characters?.find(character => character.chats?.includes(chat)) : undefined
    const bound = owner ? chat : undefined
    if (bound) {
        for (const key of Object.keys(variables)) if (key.startsWith('toggle_')) delete variables[key]
        for (const [key, value] of Object.entries(bound.savedToggleValues)) if (key.startsWith('toggle_')) variables[key] = value
    }
    if (!equal(db.globalChatVariables, variables)) db.globalChatVariables = variables
    current.variables = { ...variables }
    current.binding = owner ? { characterId: owner.chaId, conversationId: chat!.id! } : undefined
}

export function prepareImportedIdentityState(db: Database): void {
    for (const records of [db.botPresets, db.personas]) {
        const used = new Set<string>()
        for (const record of records ?? []) {
            if (typeof record.id !== 'string' || !record.id || used.has(record.id)) record.id = v4()
            used.add(record.id)
        }
    }
    const preset = db.botPresets?.[db.botPresetsId] as unknown as Snapshot
    if (preset) for (const [key, field] of Object.entries(presetMirrorMap)) {
        if (Object.hasOwn(db, key)) preset[field] = clone(root(db)[key])
    }
    const persona = db.personas?.[db.selectedPersona] as unknown as Snapshot
    if (persona) for (const [key, field] of Object.entries(personaMirrorMap)) {
        if (Object.hasOwn(db, key)) persona[field] = clone(root(db)[key])
    }
    db.explicitGlobalChatVariables = { ...db.globalChatVariables }
    for (const [key, flag] of Object.entries(protectedPresetGroups)) if (root(db)[flag]) {
        db.protectedPresetValues ??= {}
        db.protectedPresetValues[key as ProtectedPresetField] = clone(root(db)[key])
    }
    states.delete(db)
}

type UnitIntent = { key: string; type: 'set'; value: unknown } | { key: string; type: 'delete' }
export function translateRootUnitIntents(db: Database, mutations: readonly UnitIntent[]): UnitIntent[] {
    const result: UnitIntent[] = []
    const current = state(db)
    const rootIntents = new Map(mutations.map(mutation => [mutation.key, mutation]))
    const bound = boundConversation(db, current.binding)
    const boundTarget = (): boolean => {
        if (current.binding && !bound) throw new Error('Bound conversation was not found')
        return Boolean(bound)
    }
    const boundToggles = { ...bound?.savedToggleValues }
    let boundTogglesChanged = false
    const requestedValue = (field: string): unknown => {
        const mutation = rootIntents.get(JSON.stringify(['root', field]))
        return mutation ? mutation.type === 'set' ? mutation.value : undefined : root(db)[field]
    }
    for (const mutation of mutations) {
        const parts: unknown = JSON.parse(mutation.key)
        if (Array.isArray(parts) && parts[0] === 'toggle' && parts.length === 2 && boundTarget()) {
            if (mutation.type === 'delete') delete boundToggles[parts[1]]
            else boundToggles[parts[1]] = mutation.value as string
            boundTogglesChanged = true
            continue
        }
        if (!Array.isArray(parts) || parts[0] !== 'root' || parts.length !== 2) {
            result.push(mutation)
            continue
        }
        const field = String(parts[1])
        let key = mutation.key
        if (field === 'botPresetsId' || field === 'selectedPersona') {
            if (mutation.type === 'set' && typeof mutation.value === 'number') {
                const records = field === 'botPresetsId' ? db.botPresets : db.personas
                const id = records[mutation.value]?.id
                if (!id) throw new Error('Selected record was not found')
                result.push({ ...mutation, value: id })
            } else result.push(mutation)
            continue
        }
        if (Object.hasOwn(presetMirrorMap, field)) {
            const flag = protectedPresetGroups[field as ProtectedPresetField]
            if (flag && requestedValue(flag)) key = JSON.stringify(['preset-protected', field])
            else {
                const id = current.presetId ?? getEffectivePresetId(db)
                if (!id) throw new Error('Effective preset was not found')
                key = JSON.stringify(['preset', id, presetMirrorMap[field]])
            }
        } else if (Object.hasOwn(personaMirrorMap, field)) {
            const id = current.personaId ?? db.personas?.[db.selectedPersona]?.id
            if (!id) throw new Error('Effective persona was not found')
            key = JSON.stringify(['persona', id, personaMirrorMap[field as keyof typeof personaMirrorMap]])
        } else if (field === 'globalChatVariables') {
            const incoming = mutation.type === 'set' ? mutation.value as Record<string, string> : {}
            const toggles = { ...bound?.savedToggleValues }
            let boundChanged = false
            for (const variable of new Set([...Object.keys(db.globalChatVariables ?? {}), ...Object.keys(incoming)])) {
                if (incoming[variable] === db.globalChatVariables?.[variable]) continue
                if (variable.startsWith('toggle_') && boundTarget()) {
                    if (incoming[variable] === undefined) delete toggles[variable]
                    else toggles[variable] = incoming[variable]
                    boundChanged = true
                } else {
                    const variableKey = JSON.stringify([variable.startsWith('toggle_') ? 'toggle' : 'variable', variable])
                    result.push(incoming[variable] === undefined ? { key: variableKey, type: 'delete' }
                        : { key: variableKey, type: 'set', value: incoming[variable] })
                }
            }
            if (boundChanged) result.push({ key: JSON.stringify(['conversation', current.binding!.characterId,
                current.binding!.conversationId, 'savedToggleValues']), type: 'set', value: toggles })
            continue
        }
        result.push({ ...mutation, key })
        if (mutation.type === 'set' && Object.values(protectedPresetGroups).includes(field as never)
            && Boolean(mutation.value) !== Boolean(root(db)[field])) {
            for (const [protectedField, flag] of Object.entries(protectedPresetGroups)) if (flag === field) {
                const id = getEffectivePresetId(db)
                if (!mutation.value && !id) throw new Error('Effective preset was not found')
                const protectedKey = JSON.stringify(mutation.value ? ['preset-protected', protectedField]
                    : ['preset', id, presetMirrorMap[protectedField]])
                const value = requestedValue(protectedField)
                result.push(value === undefined ? { key: protectedKey, type: 'delete' }
                    : { key: protectedKey, type: 'set', value: clone(value) })
            }
        }
    }
    if (boundTogglesChanged) result.push({ key: JSON.stringify(['conversation', current.binding!.characterId,
        current.binding!.conversationId, 'savedToggleValues']), type: 'set', value: boundToggles })
    return [...new Map(result.map(mutation => [mutation.key, mutation])).values()]
}
