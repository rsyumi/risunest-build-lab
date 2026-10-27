import type { Chat, Database, Message, character, customscript } from './storage/database.svelte'
import { effectiveAuthorNote } from './authorNotePresets'
import { CONVERSATION_RANGE_MAX_LIMIT, type DataRevision } from './storage/persistentDataStore'
import type { simpleCharacterArgument } from './parser/parser.svelte'
import type { ProcessScriptCaptureContext } from './process/scripts'
import {
    planChatParserHistory, createLiveChatParserIndirections,
} from './chatParserHistory'
import { prepareCbsHistory } from './prepareCbsHistory'
import type { CbsHistoryAccess } from './cbsHistoryAccess'
import rfdc from 'rfdc'

const cloneScreenshotData = rfdc()

export type ScreenshotRange = Readonly<{ start: number; end: number }>

export type ScreenshotRangeValidation =
    | Readonly<{ ok: true; start: number; end: number }>
    | Readonly<{
          ok: false
          reason: 'empty' | 'integer' | 'bounds' | 'order'
      }>

export type DeepReadonly<T> = T extends (...args: never[]) => unknown
    ? T
    : T extends readonly unknown[]
      ? { readonly [K in keyof T]: DeepReadonly<T[K]> }
      : T extends object
        ? { readonly [K in keyof T]: DeepReadonly<T[K]> }
        : T

export interface ChatScreenshotRenderSettings {
    autoTranslate: boolean
    autoTranslateCachedOnly: boolean
    translatorType: string
    translateBeforeHTMLFormatting: boolean
    legacyTranslation: boolean
    showTranslationLoading: boolean
    newImageHandlingBeta: boolean
    assetWidth: number
    hideAllImages: boolean
    iconSize: number
    zoomSize: number
    lineHeight: number
    dynamicAssets: boolean
    dynamicAssetsEditDisplay: boolean
    legacyMediaFindings: boolean
    assetMaxDifference: number
    theme?: string
    guiHTML?: string
    roundIcons?: boolean
    hideIcons?: boolean
    proseInvert?: boolean
    requestInfoInsideChat?: boolean
    aiLawApplies?: boolean
    translator?: string
    showFirstMessagePages?: boolean
    memoryLimitThickness?: number
    customQuotes?: boolean
    customQuotesData?: [string, string, string, string]
    unformatQuotes?: boolean
    blockquoteStyling?: boolean
    returnCSSError?: boolean
}

export interface ChatScreenshotRenderContext {
    character: simpleCharacterArgument | null
    characterName: string
    characterImageSource: string
    characterLargePortrait: boolean
    userName: string
    userImageSource: string
    userLargePortrait: boolean
    moduleAssets: [string, string, string][]
    presetRegex: customscript[]
    moduleRegexScripts: customscript[]
    assetStyle: string
    parserContext: ProcessScriptCaptureContext['parserContext']
    totalTurns?: number
    selectionStart?: number
    historyStartIndex?: number
    firstParserMessageIndex?: number
    preparedDisplayText?: Record<number, { source: string; text: string }>
    settings: ChatScreenshotRenderSettings
}

export type FrozenChatScreenshotRenderContext = DeepReadonly<ChatScreenshotRenderContext>

export interface ChatScreenshotJob {
    readonly characterId: string
    readonly chatId: string
    readonly totalTurns: number
    readonly start: number
    readonly end: number
    readonly messages: readonly DeepReadonly<Message>[]
    readonly renderContext: FrozenChatScreenshotRenderContext
}

export interface ChatScreenshotDialogSnapshot {
    readonly characterId: string
    readonly chatId: string
    readonly revision: DataRevision
    readonly sessionVersion: number
    readonly totalTurns: number
    readonly renderContext: FrozenChatScreenshotRenderContext
}

export interface ChatScreenshotRangeReader {
    readonly characterId: string
    readonly chatId: string
    readonly revision: DataRevision
    readonly totalTurns: number
    readRange(startIndex: number, limit: number, signal?: AbortSignal): Promise<Message[]>
    readCharacter?(characterId: string, signal?: AbortSignal): Promise<character | null>
}

export function validateScreenshotRange(
    totalTurns: number,
    start: number,
    end: number,
): ScreenshotRangeValidation {
    if (totalTurns === 0) return { ok: false, reason: 'empty' }
    if (!Number.isInteger(start) || !Number.isInteger(end)) {
        return { ok: false, reason: 'integer' }
    }
    if (start < 1 || end < 1 || start > totalTurns || end > totalTurns) {
        return { ok: false, reason: 'bounds' }
    }
    if (start > end) return { ok: false, reason: 'order' }
    return { ok: true, start, end }
}

export function recentScreenshotRange(totalTurns: number): ScreenshotRange {
    return Object.freeze({ start: Math.max(1, totalTurns - 49), end: totalTurns })
}

export function fullScreenshotRange(totalTurns: number): ScreenshotRange {
    return Object.freeze({ start: 1, end: totalTurns })
}

/** The captured chat keeps the author's note it reads now, so parsing it needs no preset list. */
export function snapshotChatScreenshotCharacter(
    source: character,
    chat: Chat,
    authorNotePresets: Database['authorNotePresets'],
): character {
    const captureChat: Chat = {
        message: [],
        note: effectiveAuthorNote(authorNotePresets, chat),
        name: chat.name ?? '',
        localLore: chat.localLore ?? [],
        scriptstate: chat.scriptstate ?? {},
        modules: chat.modules ?? [],
        id: chat.id,
        bindedPersona: chat.bindedPersona,
        fmIndex: chat.fmIndex ?? -1,
        bookmarks: chat.bookmarks ?? [],
        bookmarkNames: chat.bookmarkNames ?? {},
        useLocallySetGlobalVariables: chat.useLocallySetGlobalVariables,
        GLGlobalVariables: chat.GLGlobalVariables ?? {},
    }
    const shared = {
        type: source.type,
        name: source.name,
        nickname: source.nickname,
        chaId: source.chaId,
        firstMessage: source.firstMessage ?? '',
        alternateGreetings: source.alternateGreetings ?? [],
        chats: [captureChat],
        chatPage: 0,
        customscript: source.customscript ?? [],
        virtualscript: source.virtualscript,
        globalLore: source.globalLore ?? [],
        defaultVariables: source.defaultVariables ?? '',
        additionalAssets: source.additionalAssets ?? [],
        emotionImages: source.emotionImages ?? [],
        prebuiltAssetStyle: source.prebuiltAssetStyle ?? '',
        prebuiltAssetCommand: source.prebuiltAssetCommand ?? false,
        prebuiltAssetExclude: source.prebuiltAssetExclude ?? [],
    }

    return {
        ...shared,
        type: 'character',
        desc: source.desc ?? '',
        personality: source.personality ?? '',
        scenario: source.scenario ?? '',
        exampleMessage: source.exampleMessage ?? '',
        systemPrompt: source.systemPrompt ?? '',
        postHistoryInstructions: source.postHistoryInstructions ?? '',
        translatorNote: source.translatorNote ?? '',
        triggerscript: source.triggerscript ?? [],
    } as character
}

function deepFreeze<T>(value: T): DeepReadonly<T> {
    if (value && typeof value === 'object' && !Object.isFrozen(value)) {
        Object.freeze(value)
        for (const child of Object.values(value)) deepFreeze(child)
    }
    return value as DeepReadonly<T>
}

export function createChatScreenshotDialogSnapshot(input: {
    characterId: string
    chatId: string
    revision: DataRevision
    sessionVersion: number
    totalTurns: number
    renderContext: ChatScreenshotRenderContext
}): ChatScreenshotDialogSnapshot {
    const renderContext = cloneScreenshotData(input.renderContext)
    renderContext.totalTurns = input.totalTurns
    return deepFreeze({
        characterId: input.characterId,
        chatId: input.chatId,
        revision: input.revision,
        sessionVersion: input.sessionVersion,
        totalTurns: input.totalTurns,
        renderContext,
    })
}

export async function createChatScreenshotJobFromDialogSnapshot(
    snapshot: ChatScreenshotDialogSnapshot,
    reader: ChatScreenshotRangeReader,
    start: number,
    end: number,
    signal?: AbortSignal,
    parseFirstPass?: typeof import('./parser/parser.svelte').risuChatParser,
): Promise<ChatScreenshotJob> {
    assertScreenshotReaderMatches(snapshot, reader)
    const validation = validateScreenshotRange(snapshot.totalTurns, start, end)
    if (validation.ok === false) throw new Error(`Invalid screenshot range: ${validation.reason}`)
    const selectionStartIndex = validation.start - 1
    const selectionEndExclusive = validation.end
    const selectedMessages = await readScreenshotRange(
        reader,
        selectionStartIndex,
        selectionEndExclusive,
        signal,
    )
    const renderContext = cloneScreenshotData(
        snapshot.renderContext,
    ) as ChatScreenshotRenderContext
    const parser = renderContext.parserContext
    const indirections = createLiveChatParserIndirections(parser.database, parser.character,
        parser.character.chats[parser.character.chatPage], parser.personaPrompt)
    const planFor = (source: unknown) => planChatParserHistory({ source: [source,
        renderContext.settings.guiHTML, renderContext.assetStyle], indirections })
    let plan = planFor(selectedMessages)
    let historyAccess: CbsHistoryAccess | undefined
    const prepare = () => prepareCbsHistory({
        characterId: snapshot.characterId, conversationId: snapshot.chatId,
        totalMessages: snapshot.totalTurns, chatID: selectionStartIndex,
        chatIDs: selectedMessages.map((_, index) => selectionStartIndex + index), plan, signal,
        reader: {
            revision: reader.revision,
            readResidentMessage: (index) => selectedMessages[index - selectionStartIndex] ?? historyAccess?.peek(index),
            readConversationWindow: async (query) => {
                const startIndex = query.startIndex!
                const endIndex = startIndex + query.limit!
                return { revision: reader.revision, value: {
                    characterId: snapshot.characterId, conversationId: snapshot.chatId,
                    startIndex, endIndex, totalMessages: snapshot.totalTurns,
                    hasMoreBefore: startIndex > 0, hasMoreAfter: endIndex < snapshot.totalTurns,
                    messages: await readScreenshotRange(reader, startIndex, endIndex, signal),
                } }
            },
        },
    })
    historyAccess = await prepare()
    const preparedDisplayText: NonNullable<ChatScreenshotRenderContext['preparedDisplayText']> = {}
    if (parseFirstPass && !plan.requiresFullHistory && selectedMessages.some((message) => /\{\{|<(?:char|user|bot)>/i.test(message.data))) {
        for (const [offset, message] of selectedMessages.entries()) {
            const chatID = selectionStartIndex + offset
            const context = renderContext.parserContext
            const text = parseFirstPass(message.data, {
                ...context, db: context.database, chara: context.character, chatID, historyAccess,
                role: message.role, rmVar: true, visualize: true,
                cbsConditions: { firstmsg: false, chatRole: message.role },
            })
            historyAccess.assertComplete()
            preparedDisplayText[chatID] = { source: message.data, text }
        }
        plan = planFor([selectedMessages, preparedDisplayText])
        historyAccess = await prepare()
        if (!plan.requiresFullHistory) {
            // A row returned by the second CBS pass may supply encoded style CBS.
            plan = planFor([selectedMessages, preparedDisplayText, [...historyAccess.preparedMessages()]])
            historyAccess = await prepare()
        }
        renderContext.preparedDisplayText = preparedDisplayText
    }
    const parserMessages = plan.requiresFullHistory ? [...historyAccess.all()] : selectedMessages
    const historyStart = plan.requiresFullHistory ? 0 : selectionStartIndex
    const selectedOffset = selectionStartIndex - historyStart
    const parserCharacter = renderContext.parserContext.character
    parserCharacter.chats[parserCharacter.chatPage].message = parserMessages
    renderContext.parserContext.database.characters[renderContext.parserContext.selectedCharID] = parserCharacter
    await hydrateScreenshotSpeakerCharacters(reader, parserMessages, renderContext, signal)
    renderContext.parserContext.historyOffset = historyStart
    renderContext.parserContext.historyAccess = historyAccess
    const preparedRows = new Map(historyAccess.preparedRows())
    renderContext.parserContext.prepareHistory = async (source, chatID, complete) => {
        const access = await prepareCbsHistory({
            characterId: snapshot.characterId, conversationId: snapshot.chatId, totalMessages: snapshot.totalTurns, chatID,
            plan: complete ? { ...planFor(source), requiresFullHistory: true } : planFor(source), signal,
            reader: {
                revision: reader.revision,
                readResidentMessage: (index) => selectedMessages[index - selectionStartIndex] ?? preparedRows.get(index),
                readConversationWindow: async (query) => {
                    const startIndex = query.startIndex!, endIndex = startIndex + query.limit!
                    return { revision: reader.revision, value: { characterId: snapshot.characterId, conversationId: snapshot.chatId,
                        startIndex, endIndex, totalMessages: snapshot.totalTurns,
                        hasMoreBefore: startIndex > 0, hasMoreAfter: endIndex < snapshot.totalTurns,
                        messages: await readScreenshotRange(reader, startIndex, endIndex, signal) } }
                },
            },
        })
        for (const [index, message] of access.preparedRows()) preparedRows.set(index, message)
        return access
    }
    renderContext.totalTurns = snapshot.totalTurns
    renderContext.selectionStart = validation.start
    renderContext.historyStartIndex = historyStart
    renderContext.firstParserMessageIndex = selectedOffset
    return deepFreeze({
        characterId: snapshot.characterId,
        chatId: snapshot.chatId,
        totalTurns: snapshot.totalTurns,
        start: validation.start,
        end: validation.end,
        messages: selectedMessages,
        renderContext,
    })
}

async function hydrateScreenshotSpeakerCharacters(
    reader: ChatScreenshotRangeReader,
    parserMessages: Message[],
    renderContext: ChatScreenshotRenderContext,
    signal?: AbortSignal,
): Promise<void> {
    if (!reader.readCharacter) return
    const characters = renderContext.parserContext.database.characters
    const knownIds = new Set(characters.map((candidate) => candidate.chaId))
    const missingIds = new Set(
        parserMessages
            .map((message) => message.saying)
            .filter(
                (id): id is string =>
                    typeof id === 'string' && id.length > 0 && !knownIds.has(id),
            ),
    )
    for (const characterId of missingIds) {
        assertScreenshotNotAborted(signal)
        const character = await reader.readCharacter(characterId, signal)
        assertScreenshotNotAborted(signal)
        if (character && !knownIds.has(character.chaId)) {
            characters.push(character)
            knownIds.add(character.chaId)
        }
    }
}


function assertScreenshotNotAborted(signal?: AbortSignal): void {
    if (signal?.aborted) {
        throw new DOMException('Screenshot capture was cancelled', 'AbortError')
    }
}

function assertScreenshotReaderMatches(
    snapshot: ChatScreenshotDialogSnapshot,
    reader: ChatScreenshotRangeReader,
): void {
    if (
        reader.characterId !== snapshot.characterId
        || reader.chatId !== snapshot.chatId
        || reader.revision !== snapshot.revision
        || reader.totalTurns !== snapshot.totalTurns
    ) {
        throw new Error('Screenshot conversation reader does not match the dialog snapshot')
    }
}

async function readScreenshotRange(
    reader: ChatScreenshotRangeReader,
    startIndex: number,
    endIndex: number,
    signal?: AbortSignal,
): Promise<Message[]> {
    const messages: Message[] = []
    for (let cursor = startIndex; cursor < endIndex;) {
        assertScreenshotNotAborted(signal)
        const limit = Math.min(CONVERSATION_RANGE_MAX_LIMIT, endIndex - cursor)
        const page = await reader.readRange(cursor, limit, signal)
        assertScreenshotNotAborted(signal)
        if (page.length !== limit) {
            throw new Error(
                `Screenshot conversation range ${cursor}:${cursor + limit} returned ${page.length} messages`,
            )
        }
        messages.push(...page)
        cursor += limit
    }
    return messages
}
