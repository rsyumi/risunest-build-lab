import type { Database, character, Chat } from './storage/database.svelte'

export type ChatParserUnsafeHistoryDependency =
    | 'lua'
    | 'plugin-v2'
    | 'display-trigger'
    | 'inject'
    | 'repeat-back'
    | 'regex-reparse'

export type ChatParserHistoryReason =
    | ChatParserUnsafeHistoryDependency
    | 'full-history-cbs'
    | 'dynamic-previous-chat-log'
    | 'ambiguous-risu-style'
    | 'opaque-cbs'

export interface ChatParserHistoryClassification {
    readonly requiresFullHistory: boolean
    readonly absoluteMessageIndices: readonly number[]
    readonly reasons: readonly ChatParserHistoryReason[]
}

export interface ChatParserHistoryClassificationInput {
    readonly source: unknown
    readonly unsafeDependencies?: readonly ChatParserUnsafeHistoryDependency[]
    readonly indirections?: Readonly<Record<string, unknown>>
}

export type CbsHistoryRequirement = 'count' | 'current' | 'tail' | 'previouschar' | 'previoususer' | 'idle'

export interface CbsHistoryPlan extends ChatParserHistoryClassification {
    readonly requirements: readonly CbsHistoryRequirement[]
}

const HISTORY_REQUIREMENTS: Readonly<Record<string, CbsHistoryRequirement>> = {
    lastmessageid: 'count', lastmessageindex: 'count', pick: 'count', rollp: 'count', rollpick: 'count',
    messagetime: 'current', messagedate: 'current', role: 'current',
    lastmessage: 'tail', idleduration: 'tail',
    previouscharchat: 'previouschar', lastcharmessage: 'previouschar',
    previoususerchat: 'previoususer', lastusermessage: 'previoususer',
    messageidleduration: 'idle',
}

const FULL_HISTORY_CBS_NAMES = new Set([
    'userhistory',
    'usermessages',
    'charhistory',
    'charmessages',
    'history',
    'messages',
    'messageunixtimearray',
    'idleduration',
    'lastmessage',
    'lastmessageid',
    'lastmessageindex',
    'pick',
    'rollp',
    'rollpick',
])

const PREVIOUS_CHAT_LOG_CBS_NAME = 'previouschatlog'

export function classifyChatParserHistory(
    input: ChatParserHistoryClassificationInput,
): ChatParserHistoryClassification {
    const { requirements, ...classification } = buildHistoryPlan(input, false)
    return classification
}

export function planChatParserHistory(input: ChatParserHistoryClassificationInput): CbsHistoryPlan {
    return buildHistoryPlan(input, true)
}

function buildHistoryPlan(input: ChatParserHistoryClassificationInput, precise: boolean): CbsHistoryPlan {
    const reasons = new Set<ChatParserHistoryReason>(input.unsafeDependencies ?? [])
    const absoluteMessageIndices = new Set<number>()
    const texts = collectParserInputText(input.source)
    const indirections = new Map(
        Object.entries(input.indirections ?? {}).map(([name, value]) => [
            normalizeCbsName(name),
            value,
        ]),
    )
    const followedIndirections = new Set<string>()
    const requirements = new Set<CbsHistoryRequirement>()

    for (const text of texts) {
        classifyText(
            text,
            reasons,
            absoluteMessageIndices,
            true,
            indirections,
            followedIndirections,
            requirements,
            precise,
        )
    }

    return {
        requiresFullHistory: reasons.size > 0,
        absoluteMessageIndices: [...absoluteMessageIndices].sort((a, b) => a - b),
        reasons: [...reasons],
        requirements: [...requirements],
    }
}

export function extendChatParserHistoryBounds(
    classification: ChatParserHistoryClassification,
    bounds: Readonly<{ start: number; end: number; totalMessages: number }>,
): Readonly<{ start: number; end: number }> {
    if (classification.requiresFullHistory) {
        return { start: 0, end: bounds.totalMessages }
    }

    let start = bounds.start
    let end = bounds.end
    for (const requestedIndex of classification.absoluteMessageIndices) {
        if (requestedIndex < 0 || requestedIndex >= bounds.totalMessages) continue
        start = Math.min(start, requestedIndex)
        end = Math.max(end, requestedIndex + 1)
    }
    return { start, end }
}

function classifyText(
    text: string,
    reasons: Set<ChatParserHistoryReason>,
    absoluteMessageIndices: Set<number>,
    decodeStyles: boolean,
    indirections: ReadonlyMap<string, unknown>,
    followedIndirections: Set<string>,
    requirements: Set<CbsHistoryRequirement>,
    precise: boolean,
): void {
    if (/@@repeat_back|<repeat_back(?:>|:)/.test(text)) reasons.add('repeat-back')
    for (const expression of extractCbsExpressions(text)) {
        const parsed = parseCbsExpression(expression)
        const requirement = HISTORY_REQUIREMENTS[parsed.name]
        if (requirement) requirements.add(requirement)
        if (hasDynamicCbsName(expression) || parsed.name === 'call' || parsed.name.startsWith('#each')) {
            reasons.add('opaque-cbs')
        }
        if (FULL_HISTORY_CBS_NAMES.has(parsed.name) && (!precise || !requirement)) {
            reasons.add('full-history-cbs')
            continue
        }
        if (parsed.name === PREVIOUS_CHAT_LOG_CBS_NAME) {
            if (!parsed.firstArgument || !/^\d+$/.test(parsed.firstArgument)) {
                reasons.add('dynamic-previous-chat-log')
            } else {
                absoluteMessageIndices.add(Number(parsed.firstArgument))
            }
        }
        if (
            !indirections.has(parsed.name) ||
            followedIndirections.has(parsed.name)
        ) continue
        followedIndirections.add(parsed.name)
        for (const indirectText of collectParserInputText(indirections.get(parsed.name))) {
            classifyText(
                indirectText,
                reasons,
                absoluteMessageIndices,
                true,
                indirections,
                followedIndirections,
                requirements,
                precise,
            )
        }
    }

    if (!decodeStyles || !text.toLocaleLowerCase().includes('<risu-style')) return

    const stylePattern = /<risu-style(?:\s[^>]*)?>([\s\S]*?)<\/risu-style\s*>/gi
    let textWithoutMatchedStyles = text
    for (const match of text.matchAll(stylePattern)) {
        const decoded = decodeHexUtf8(match[1])
        if (decoded === null) {
            reasons.add('ambiguous-risu-style')
        } else {
            classifyText(
                decoded,
                reasons,
                absoluteMessageIndices,
                false,
                indirections,
                followedIndirections,
                requirements,
                precise,
            )
        }
    }
    textWithoutMatchedStyles = textWithoutMatchedStyles.replace(stylePattern, '')
    if (/<risu-style\b/i.test(textWithoutMatchedStyles)) {
        reasons.add('ambiguous-risu-style')
    }
}

function extractCbsExpressions(text: string): string[] {
    const starts: number[] = []
    const expressions: string[] = []
    let pureDepth = 0
    for (let index = 0; index < text.length - 1; index += 1) {
        if (text[index] === '{' && text[index + 1] === '{') {
            starts.push(index + 2)
            index += 1
            continue
        }
        if (text[index] !== '}' || text[index + 1] !== '}' || starts.length === 0) continue
        const expression = text.slice(starts.pop(), index)
        if (!pureDepth || expression.startsWith('call::')) expressions.push(expression)
        if (starts.length === 0) {
            if (pureDepth) {
                if ((expression.startsWith('#') || expression.startsWith(':')) && expression !== ':else') pureDepth++
                else if (expression.startsWith('/') && !expression.startsWith('//')) pureDepth--
            } else if (expression === '#pure' || expression === '#pure_display' || expression === '#puredisplay'
                || expression.startsWith('#escape') || expression.startsWith('#func ')) pureDepth = 1
        }
        index += 1
    }
    return expressions
}

function parseCbsExpression(expression: string): Readonly<{
    name: string
    firstArgument: string | null
}> {
    const colonIndex = expression.indexOf(':')
    const rawName = colonIndex === -1 ? expression : expression.slice(0, colonIndex)
    const name = normalizeCbsName(rawName)
    if (colonIndex === -1) return { name, firstArgument: null }

    const doubleColon = expression[colonIndex + 1] === ':'
    const argumentStart = colonIndex + (doubleColon ? 2 : 1)
    const argumentText = expression.slice(argumentStart)
    const nextSeparator = argumentText.indexOf(doubleColon ? '::' : ':')
    return {
        name,
        firstArgument: (nextSeparator === -1
            ? argumentText
            : argumentText.slice(0, nextSeparator)
        ).trim(),
    }
}

function hasDynamicCbsName(expression: string): boolean {
    const colonIndex = expression.indexOf(':')
    const rawName = colonIndex === -1 ? expression : expression.slice(0, colonIndex)
    if (!rawName.includes('{{')) return false
    // Blocks and `? ` arithmetic dispatch on their leading keyword, so a nested
    // condition or operand cannot select another macro. Other names drop
    // whitespace before lookup and stay dynamic.
    if (rawName.startsWith('#') || rawName.startsWith('? ')) return /^\S*\{\{/.test(rawName)
    return true
}

function normalizeCbsName(name: string): string {
    return name.toLocaleLowerCase().replace(/[\s_-]/g, '')
}

function decodeHexUtf8(value: string): string | null {
    if (!/^(?:[0-9a-fA-F]{2})*$/.test(value)) return null
    const bytes = new Uint8Array(value.length / 2)
    for (let index = 0; index < bytes.length; index += 1) {
        bytes[index] = Number.parseInt(value.slice(index * 2, index * 2 + 2), 16)
    }
    try {
        return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
    } catch {
        return null
    }
}

function collectParserInputText(
    value: unknown,
    seen = new WeakSet<object>(),
    output: string[] = [],
): string[] {
    if (typeof value === 'string') {
        output.push(value)
        return output
    }
    if (!value || typeof value !== 'object' || seen.has(value)) return output
    seen.add(value)
    for (const child of Object.values(value)) {
        collectParserInputText(child, seen, output)
    }
    return output
}

export function createLiveChatParserIndirections(
    database: Database,
    selectedCharacter: character,
    conversation: Chat,
    personaPrompt: string,
): Readonly<Record<string, unknown>> {
    const character = selectedCharacter
    let authorNote = conversation.note ?? ''
    if (!authorNote) {
        for (const item of database.promptTemplate ?? []) {
            if (item.type !== 'authornote' || !item.defaultText) continue
            authorNote = item.defaultText
            break
        }
    }
    return {
        personality: character?.personality ?? '',
        charpersona: character?.personality ?? '',
        description: character?.desc ?? '',
        chardesc: character?.desc ?? '',
        scenario: character?.scenario ?? '',
        exampledialogue: character?.exampleMessage ?? '',
        examplemessage: character?.exampleMessage ?? '',
        persona: personaPrompt,
        userpersona: personaPrompt,
        mainprompt: database.mainPrompt ?? '',
        systemprompt: database.mainPrompt ?? '',
        jb: database.jailbreak ?? '',
        jailbreak: database.jailbreak ?? '',
        globalnote: database.globalNote ?? '',
        systemnote: database.globalNote ?? '',
        ujb: database.globalNote ?? '',
        authornote: authorNote,
    }
}
