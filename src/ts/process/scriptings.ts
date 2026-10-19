import { asBuffer } from 'src/ts/util';
import {
    getChatVar,
    getChatVarFromConversation,
    getGlobalChatVar,
    setChatVar,
    setChatVarOnConversation,
} from "../parser/chatVar.svelte";
import { hasher, type simpleCharacterArgument, risuChatParser } from "../parser/parser.svelte";
import type { LuaEngine, LuaFactory } from "wasmoon";
import { getCurrentCharacter, getCurrentChat, getDatabase, setDatabase, type Chat, type Database, type Message, type character, type triggerscript } from "../storage/database.svelte";
import { get } from "svelte/store";
import { DBState, ReloadChatPointer, ReloadGUIPointer, selectedCharID } from "../stores.svelte";
import { alertSelect, alertError, alertInput, alertNormal, alertConfirm } from "../alert";
import { HypaProcesser } from "./memory/hypamemory";
import { generateAIImage } from "./stableDiff";
import { writeInlayImage, getInlayAsset } from "./files/inlays";
import type { OpenAIChat, MultiModal } from "./index.svelte";
import { requestChatData, type StreamResponseChunk } from "./request/request";
import { v4 } from "uuid";
import { getModuleLorebooks, getModuleTriggers } from "./modules";
import { Mutex } from "../mutex";
import { tokenize } from "../tokenizer";
import { fetchNative, readImage } from "../globalApi.svelte";
import { loadLoreBookV3PromptFromCompatibilitySnapshot } from './lorebook.svelte';
import { getPersonaPrompt, getUserName, getUserIcon } from '../util';
import { isTauriMobile } from '../platform';
import { getRuntimePerformanceBudgets, subscribeRuntimePerformanceProfile } from '../runtimePerformanceProfile';
import { createLuaFactory, runLuaSource } from './luaRuntime';
import { withObjectUrl } from '../objectUrl';
import {
    type ConversationOperationContext,
    type ConversationCommitObserver,
} from './conversationOperationContext';
import { peekActiveConversationSession } from '../storage/persistentDataRuntime.svelte';
import type { ActiveConversationSession } from '../storage/activeConversationSession';
import { runSerializedUserTrigger } from './conversationUserTrigger';
import {
    findActiveHistoryWindow,
    getHistoryMessageAt,
    historyLength,
    sliceHistory,
    toHistorySpliceIndex,
} from './historyWindowIndex';
import { openHistoryWindowCopy, type HistoryWindowCopy } from './historyWindowWrite';
import { PersistentMutationFencedError } from '../storage/saveCoordinator';
import {
    createScriptConversationAccess,
    type ScriptConversationAccess,
} from './scriptConversationAccess';
import { requestScriptDisplayRefresh } from './scriptDisplayRefresh';
import { beginDisplayScriptRun, noticeScriptTimeout } from './scriptTimeoutNotice';
import { findCharacterById, findCharacterIndexById } from './scriptCharacterLookup';

/** Deadline for each synchronous Lua segment of a listener. */
const LUA_LISTENER_TIMEOUT_MS = 2_000
/** Deadline for each synchronous Lua segment of a handler the user started. */
const LUA_USER_HANDLER_TIMEOUT_MS = 60_000
let luaFactory:LuaFactory
let luaTimeoutErrorConstructor: typeof import('wasmoon').LuaTimeoutError | undefined
let ScriptingSafeIds = new Set<string>()
let ScriptingEditDisplayIds = new Set<string>()
let ScriptingLowLevelIds = new Set<string>()
let lastRequestResetTime = 0
let lastRequestsCount = 0

type CapturedCharacterField = 'name'|'desc'|'firstMessage'|'backgroundHTML'

interface CapturedCharacterOwner {
    database: Database
    character: character
    fields: Record<CapturedCharacterField, string|undefined>
}

interface BasicScriptingEngineState {
    code?: string;
    mutex: Mutex;
    cacheKey: string;
    activeUses: number;
    lastUsed: number;
    chat?: Chat;
    setVar?: (key:string, value:string) => boolean|void,
    getVar?: (key:string) => string,
    operationDatabase?: ReturnType<ConversationOperationContext['createDatabaseView']>
    operationCharacter?: character|simpleCharacterArgument
    selectedCharacterId?: string
    capturedCharacterOwner?: CapturedCharacterOwner
    ensureConversationOperation?: () => void
    access?: ScriptConversationAccess
    readChat?: () => Chat
    writeChat?: () => Chat
    readDatabase?: () => Database
    flushVariables?: () => void
    stopSending?: boolean
}

interface LuaScriptingEngineState extends BasicScriptingEngineState {
    engine?: LuaEngine;
    type: 'lua';
}

interface PythonScriptingEngineState extends BasicScriptingEngineState {
    pyodide?: PyodideContext
    type: 'py';
}

type ScriptingEngineState = LuaScriptingEngineState | PythonScriptingEngineState;

function clearScriptingEngineInvocationState(state: ScriptingEngineState) {
    state.chat = undefined
    state.setVar = undefined
    state.getVar = undefined
    state.operationDatabase = undefined
    state.operationCharacter = undefined
    state.selectedCharacterId = undefined
    state.capturedCharacterOwner = undefined
    state.ensureConversationOperation = undefined
    state.access = undefined
    state.readChat = undefined
    state.writeChat = undefined
    state.readDatabase = undefined
    state.flushVariables = undefined
    state.stopSending = undefined
}

let ScriptingEngines = new Map<string, ScriptingEngineState>()
const luaDisplayOperationMutexes = new WeakMap<ActiveConversationSession, Mutex>()
let luaFactoryPromise: Promise<void> | null = null;
let scriptingEngineRecency = 0

subscribeRuntimePerformanceProfile(() => {
    evictIdleScriptingEngines()
})

export async function runScripted(code:string, arg:{
    char?:character|simpleCharacterArgument,
    chat?:Chat
    data?: string|OpenAIChat[],
    setVar?: (key:string, value:string) => boolean|void,
    getVar?: (key:string) => string,
    lowLevelAccess?: boolean,
    meta?: object,
    mode?: string,
    type?: 'lua'|'py'
    operationContext?: ConversationOperationContext
    /** The selected conversation through a variable scope and copy-on-edit messages. */
    access?: ScriptConversationAccess
    /** A button or manual trigger the user started, which gets the longer deadline. */
    userStarted?: boolean
    createConversationChat?: () => Chat
}){
    const type: 'lua'|'py' = arg.type ?? 'lua'
    if (type === 'py' && isTauriMobile) {
        throw new Error('Python scripting is unavailable on Tauri mobile')
    }
    const char = arg.char ?? getCurrentCharacter()
    const selectedCharacterId = arg.operationContext?.characterId ?? char?.chaId
    const capturedDatabase = DBState.db
    const capturedCharacter = selectedCharacterId === undefined || !capturedDatabase.characters
        ? undefined
        : findCharacterById(capturedDatabase.characters, selectedCharacterId, get(selectedCharID))
    const capturedCharacterOwner: CapturedCharacterOwner|undefined = capturedCharacter
        ? {
            database: capturedDatabase,
            character: capturedCharacter,
            fields: {
                name: capturedCharacter.name,
                desc: capturedCharacter.desc,
                firstMessage: capturedCharacter.firstMessage,
                backgroundHTML: capturedCharacter.backgroundHTML,
            },
        }
        : undefined
    const data = arg.data ?? ''
    const access = arg.access
    const operationDatabase = arg.operationContext?.createDatabaseView(getDatabase())
    const setVar = arg.setVar ?? (access
        ? (key: string, value: string) => access.setVariable(key, value)
        : arg.operationContext
        ? (key: string, value: string) => setChatVarOnConversation(
            arg.operationContext!.chat,
            key,
            value,
        )
        : setChatVar)
    const getVar = arg.getVar ?? (access
        ? (key: string) => access.getVariable(key)
        : arg.operationContext
        ? (key: string) => getChatVarFromConversation(
            operationDatabase!,
            arg.operationContext!.characterId,
            arg.operationContext!.chat,
            key,
        )
        : getChatVar)
    const meta = arg.meta ?? {}
    const mode = arg.mode ?? 'manual'

    let chat = access?.currentChat() ?? arg.operationContext?.chat ?? arg.chat ?? getCurrentChat()
    let lowLevelAccess = arg.lowLevelAccess ?? false

    if(type === 'lua'){
        await ensureLuaFactory()
    }
    const ownerChaId = char?.chaId ?? 'global'
    const functionTimeout = arg.userStarted ? LUA_USER_HANDLER_TIMEOUT_MS : LUA_LISTENER_TIMEOUT_MS
    const ScriptingEngineState = getOrCreateEngineState(mode, type, ownerChaId, code, functionTimeout);
    ScriptingEngineState.activeUses++
    let invocationFinished = false
    
    try {
    return await ScriptingEngineState.mutex.runExclusive(async () => {
        try {
        ScriptingEngineState.chat = chat
        ScriptingEngineState.setVar = setVar
        ScriptingEngineState.getVar = getVar
        ScriptingEngineState.operationCharacter = char
        ScriptingEngineState.operationDatabase = operationDatabase
        ScriptingEngineState.selectedCharacterId = selectedCharacterId
        ScriptingEngineState.capturedCharacterOwner = capturedCharacterOwner
        let invocationChatReady = false
        ScriptingEngineState.ensureConversationOperation = () => {
            if (!arg.createConversationChat || invocationChatReady) return
            const ownedChat = arg.createConversationChat()
            invocationChatReady = true
            ScriptingEngineState.chat = ownedChat
            ScriptingEngineState.setVar = (key: string, value: string) =>
                setChatVarOnConversation(ownedChat, key, value)
            ScriptingEngineState.getVar = (key: string) =>
                getChatVarFromConversation(capturedDatabase, selectedCharacterId, ownedChat, key)
        }
        const legacyChat = () => {
            ScriptingEngineState.ensureConversationOperation?.()
            return ScriptingEngineState.chat
        }
        ScriptingEngineState.access = access
        ScriptingEngineState.readChat = access ? () => access.readChat() : legacyChat
        ScriptingEngineState.writeChat = access ? () => access.writeChat() : legacyChat
        ScriptingEngineState.readDatabase = access
            ? () => {
                access.readChat()
                return access.databaseView(getDatabase())
            }
            : () => {
                ScriptingEngineState.ensureConversationOperation?.()
                return ScriptingEngineState.operationDatabase ?? getDatabase()
            }
        ScriptingEngineState.flushVariables = access ? () => { access.flushVariables() } : undefined
        ScriptingEngineState.stopSending = false
        if (code !== ScriptingEngineState.code) {
            try {
            let declareAPI:(name: string, func:Function) => void

            if(ScriptingEngineState.type === 'lua'){
                console.log('Creating new Lua engine for mode:', mode)
                ScriptingEngineState.engine?.global.close()
                ScriptingEngineState.engine = await luaFactory.createEngine({
                    injectObjects: true,
                    functionTimeout,
                })
                const luaEngine = ScriptingEngineState.engine
                declareAPI = (name:string, func:Function) => {
                    luaEngine.global.set(name, func)
                }
            }
            if(ScriptingEngineState.type === 'py'){
                console.log('Creating new Pyodide context for mode:', mode)
                ScriptingEngineState.pyodide?.close()
                ScriptingEngineState.pyodide = new PyodideContext()
                declareAPI = (name:string, func:Function) => {
                    ScriptingEngineState.pyodide?.declareAPI(name, func as any)
                }
            }
            const ensureScriptingVariables = () => {
                if (!ScriptingEngineState.access) {
                    ScriptingEngineState.ensureConversationOperation?.()
                }
            }
            const readScriptingChat = () => ScriptingEngineState.readChat()
            const writeScriptingChat = () => ScriptingEngineState.writeChat()
            const editScriptingMessage = (index: number, edit: (message: Message) => void) => {
                if (ScriptingEngineState.access) {
                    ScriptingEngineState.access.editMessage(index, edit)
                    return
                }
                const chat = writeScriptingChat()
                const message = chat.message
                    ? getHistoryMessageAt(chat, index)
                    : undefined
                if (message) edit(message)
            }
            const flushScriptingVariables = () => ScriptingEngineState.flushVariables?.()
            const getScriptingDatabase = () => ScriptingEngineState.readDatabase?.() ?? getDatabase()
            const getScriptingCharacterIndex = (db: Database) => {
                if (ScriptingEngineState.selectedCharacterId !== undefined) {
                    return findCharacterIndexById(
                        db.characters,
                        ScriptingEngineState.selectedCharacterId,
                        get(selectedCharID),
                    )
                }
                return get(selectedCharID)
            }
            const getScriptingCharacter = () => {
                const db = getScriptingDatabase()
                return db.characters[getScriptingCharacterIndex(db)]
            }
            const updateCapturedCharacterField = (
                field: CapturedCharacterField,
                value: string,
            ) => {
                const owner = ScriptingEngineState.capturedCharacterOwner
                if (!owner || DBState.db !== owner.database) return false

                const liveCharacter = DBState.db.characters?.find(
                    (candidate) => candidate.chaId === owner.character.chaId,
                )
                if (liveCharacter !== owner.character) return false

                const liveRecord = liveCharacter as unknown as Record<
                    CapturedCharacterField,
                    string|undefined
                >
                if (!Object.is(liveRecord[field], owner.fields[field])) {
                    return false
                }

                liveRecord[field] = value
                owner.fields[field] = value
                const projectedCharacter = ScriptingEngineState.operationDatabase
                    ?.characters.find(
                        (candidate) => candidate.chaId === owner.character.chaId,
                    )
                if (projectedCharacter && projectedCharacter !== liveCharacter) {
                    const projectedRecord = projectedCharacter as unknown as Record<
                        CapturedCharacterField,
                        string|undefined
                    >
                    projectedRecord[field] = value
                }
                return true
            }
            declareAPI('getChatVar', (id:string,key:string) => {
                ensureScriptingVariables()
                return ScriptingEngineState.getVar(key)
            })
            declareAPI('setChatVar', (id:string,key:string, value:string) => {
                if(!ScriptingSafeIds.has(id) && !ScriptingEditDisplayIds.has(id)){
                    return
                }
                ensureScriptingVariables()
                ScriptingEngineState.setVar(key, value)
            })
            declareAPI('setChatVarChanged', (id:string,key:string, value:string) => {
                if(!ScriptingSafeIds.has(id) && !ScriptingEditDisplayIds.has(id)){
                    return
                }
                ensureScriptingVariables()
                if(ScriptingEngineState.setVar(key, value) === true){
                    return true
                }
            })
            declareAPI('getGlobalVar', (id:string, key:string) => {
                return getGlobalChatVar(key)
            })
            declareAPI('stopChat', (id:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                ScriptingEngineState.stopSending = true
            })
            declareAPI('alertError', (id:string, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                alertError(value)
            })
            declareAPI('alertNormal', (id:string, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                alertNormal(value)
            })
            declareAPI('alertInput', (id:string, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                flushScriptingVariables()
                return alertInput(value)
            })
            declareAPI('alertSelect', (id:string, value:string[]) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                flushScriptingVariables()
                return alertSelect(value)
            })
            declareAPI('alertConfirm', (id:string, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                flushScriptingVariables()
                return alertConfirm(value).then(res => res ? true : false)
            })

            declareAPI('getChatMain', (id:string, index:number) => {
                const chat = getHistoryMessageAt(readScriptingChat(), index)
                if(!chat){
                    return JSON.stringify(null)
                }
                const data = {
                    role: chat.role,
                    data: chat.data,
                    time: chat.time ?? 0
                }
                return JSON.stringify(data)
            })

            declareAPI('getChatData', (id:string, index:number) => {
                const chat = getHistoryMessageAt(readScriptingChat(), index)
                return chat?.data ?? ''
            })

            declareAPI('getChatRole', (id:string, index:number) => {
                const chat = getHistoryMessageAt(readScriptingChat(), index)
                return chat?.role ?? ''
            })

            declareAPI('getRecentChatsMain', (id:string, count:number) => {
                const chats = readScriptingChat().message
                const safeCount = Math.max(0, Math.floor(count || 0))
                const start = Math.max(0, chats.length - safeCount)
                return JSON.stringify(chats.slice(start).map((v) => ({
                    role: v.role,
                    data: v.data,
                    time: v.time ?? 0,
                })))
            })

            declareAPI('setChat', (id:string, index:number, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                editScriptingMessage(index, (message) => {
                    message.data = value ?? ''
                })
            })
            declareAPI('setChatRole', (id:string, index:number, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                editScriptingMessage(index, (message) => {
                    message.role = value === 'user' ? 'user' : 'char'
                })
            })
            declareAPI('cutChat', (id:string, start:number, end:number) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                const chat = writeScriptingChat()
                chat.message = sliceHistory(chat, start, end)
            })
            declareAPI('removeChat', (id:string, index:number) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                const chat = writeScriptingChat()
                const start = toHistorySpliceIndex(chat, index)
                if (start === null) return
                chat.message.splice(start, 1)
            })
            declareAPI('addChat', (id:string, role:string, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                let roleData:'user'|'char' = role === 'user' ? 'user' : 'char'
                const message = {role: roleData, data: value ?? '', chatId: v4()}
                if (ScriptingEngineState.access) {
                    ScriptingEngineState.access.appendMessage(message)
                    return
                }
                writeScriptingChat().message.push(message)
            })
            declareAPI('insertChat', (id:string, index:number, role:string, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                const chat = writeScriptingChat()
                let roleData:'user'|'char' = role === 'user' ? 'user' : 'char'
                const start = toHistorySpliceIndex(chat, index)
                if (start === null) return
                chat.message.splice(start, 0, {role: roleData, data: value ?? '', chatId: v4()})
            })

            declareAPI('getTokens', async (id:string, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                return await tokenize(value)
            })

            declareAPI('getChatLength', (id:string) => {
                return historyLength(readScriptingChat())
            })

            declareAPI('getFullChatMain', (id:string) => {
                const data = JSON.stringify(readScriptingChat().message.map((v) => {
                    return {
                        role: v.role,
                        data: v.data,
                        chatId: v.chatId,
                        time: v.time ?? 0,
                    }
                }))
                return data
            })

            declareAPI('sleep', (id:string, time:number) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                return new Promise((resolve) => {
                    setTimeout(() => {
                        resolve(true)
                    }, time)
                })
            })

            declareAPI('cbs', (value) => {
                const database = getScriptingDatabase()
                const operationCharacter = ScriptingEngineState.operationCharacter
                const parserCharacter = operationCharacter?.type === 'simple'
                    ? getScriptingCharacter()
                    : operationCharacter ?? getScriptingCharacter()
                return risuChatParser(value, {
                    chara: parserCharacter,
                    db: ScriptingEngineState.access ? database : ScriptingEngineState.operationDatabase,
                    selectedCharacterId: ScriptingEngineState.selectedCharacterId,
                    getChatVar: ScriptingEngineState.getVar,
                    setChatVar: ScriptingEngineState.setVar,
                })
            })
            
            declareAPI('setFullChatMain', (id:string, value:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                const chat = writeScriptingChat()
                const realValue = JSON.parse(value)
                const originals = new Map<string, { count: number, message: Chat['message'][number] | null }>()
                for (const message of chat.message) {
                    if (!message.chatId) continue
                    const entry = originals.get(message.chatId)
                    if (entry) {
                        entry.count += 1
                        entry.message = null
                    } else originals.set(message.chatId, { count: 1, message })
                }
                const counts = new Map<string, number>()
                for (const message of realValue) {
                    if (typeof message.chatId === 'string')
                        counts.set(message.chatId, (counts.get(message.chatId) ?? 0) + 1)
                }
                const kept = new Map<string, number>()
                chat.message = realValue.map((v) => {
                    // A stored chatId is kept for as many messages as the chat holds it, in order;
                    // further copies and unknown ids become new messages, never another output
                    // target. Plugin metadata carries over only when the id names one message on both sides.
                    const entry = typeof v.chatId === 'string' ? originals.get(v.chatId) : undefined
                    const used = entry ? kept.get(v.chatId) ?? 0 : 0
                    if (!entry || used >= entry.count) return { role: v.role, data: v.data, chatId: v4() }
                    kept.set(v.chatId, used + 1)
                    const original = counts.get(v.chatId) === 1 ? entry.message : null
                    return {
                        ...original,
                        role: v.role,
                        data: v.data,
                        chatId: v.chatId,
                    }
                })
            })

            declareAPI('logMain', (value:string) => {
                console.log(JSON.parse(value))
            })

            declareAPI('reloadDisplay', (id:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                flushScriptingVariables()
                requestScriptDisplayRefresh({ all: true })
            })

            declareAPI('reloadChat', (id: string, index: number) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                flushScriptingVariables()
                ReloadChatPointer.update((v) => {
                    v[index] = (v[index] ?? 0) + 1
                    return v
                })
            })

            //Low Level Access
            declareAPI('similarity', async (id:string, source:string, value:string[]) => {
                if(!ScriptingLowLevelIds.has(id)){
                    return
                }
                const processer = new HypaProcesser()
                await processer.addText(value)
                return await processer.similaritySearch(source)
            })

            declareAPI('request', async (id:string, url:string) => {
                if(!ScriptingLowLevelIds.has(id)){
                    return
                }

                if(lastRequestResetTime + 60000 < Date.now()){
                    lastRequestsCount = 0
                    lastRequestResetTime = Date.now()
                }
                
                if(lastRequestsCount > 5){
                    return JSON.stringify({
                        status: 429,
                        data: 'Too many requests. you can request 5 times per minute'
                    })
                }

                lastRequestsCount++

                try {
                    //for security and other reasons, only get request in 120 char is allowed
                    if(url.length > 120){
                        return JSON.stringify({
                            status: 413,
                            data: 'URL to large. max is 120 characters'
                        })
                    }

                    if(!url.startsWith('https://')){
                        return JSON.stringify({
                            status: 400,
                            data: "Only https requests are allowed"
                        })
                    }

                    const bannedURL = [
                        "https://realm.risuai.net",
                        "https://risuai.net",
                        "https://risuai.xyz"
                    ]

                    for(const burl of bannedURL){

                        if(url.startsWith(burl)){
                            return JSON.stringify({
                                status: 400,
                                data: "request to " + url + ' is not allowed'
                            })
                        }
                    }

                    //browser fetch
                    const d = await fetchNative(url, {
                        method: "GET"
                    })
                    const text = await d.text()
                    return JSON.stringify({
                        status: d.status,
                        data: text
                    })

                } catch (error) {
                    return JSON.stringify({
                        status: 400,
                        data: 'internal error'
                    })
                }
            })

            declareAPI('generateImage', async (id:string, value:string, negValue:string = '') => {
                if(!ScriptingLowLevelIds.has(id)){
                    return
                }
                const gen = await generateAIImage(
                    value,
                    ScriptingEngineState.operationCharacter as character,
                    negValue,
                    'inlay',
                )
                if(!gen){
                    return 'Error: Image generation failed'
                }
                const imgHTML = new Image()
                imgHTML.src = gen
                const inlay = await writeInlayImage(imgHTML)
                return `{{inlay::${inlay}}}`
            })

            declareAPI('getCharacterImageMain', async (id:string) => {
                try {
                    const db = getScriptingDatabase()
                    const selectedChar = getScriptingCharacterIndex(db)

                    if (selectedChar < 0 || selectedChar >= db.characters.length) {
                        return ''
                    }

                    const character = db.characters[selectedChar]
                    
                    if (!character || !character.image) {
                        return ''
                    }
                    
                    const img = await readImage(character.image)
                    const imgObj = new Image()
                    const extention = character.image.split('.').at(-1)

                    const imgid = await withObjectUrl(
                        new Blob([asBuffer(img)], {type: `image/${extention}`}),
                        (url) => writeInlayImage(
                            imgObj,
                            { name: character.image, ext: extention, id: character.image },
                            url,
                        ),
                    )

                    if (imgid) {
                        return `{{inlayed::${imgid}}}`
                    }
                    console.warn('Failed to create character image inlay')
                    return ''
                } catch (error) {
                    console.error('Error in getCharacterImageMain:', error)
                    return ''
                }
            })

            declareAPI('getPersonaImageMain', async (id:string) => {
                try {
                    const icon = getUserIcon()

                    if(!icon) {
                        return ''
                    }

                    const img = await readImage(icon)
                    const imgObj = new Image()
                    const extention = icon.split('.').at(-1)

                    const imgid = await withObjectUrl(
                        new Blob([asBuffer(img)], {type: `image/${extention}`}),
                        (url) => writeInlayImage(imgObj, { name: icon, ext: extention, id: icon }, url),
                    )

                    if (imgid) {
                        return `{{inlayed::${imgid}}}`
                    }
                    
                    console.warn('Failed to create character image inlay')
                    return ''
                } catch (error) {
                    console.error('Error in getCharacterImageMain:', error)
                    return ''
                }
            })

            declareAPI('hash', async (id:string, value:string) => {
                return await hasher(new TextEncoder().encode(value))
            })

            const parseLuaOptions = (optionsStr?: string) => {
                if (!optionsStr) {
                    return {};
                }

                try {
                    const parsed = JSON.parse(optionsStr);
                    return parsed && typeof parsed === 'object' ? parsed : {};
                } catch {
                    return {};
                }
            };

            const collectLuaStreamText = async (stream: ReadableStream<StreamResponseChunk>) => {
                const reader = stream.getReader();
                let text = '';

                try {
                    while (true) {
                        const { done, value } = await reader.read();
                        if (done) {
                            break;
                        }
                        if (value && typeof value['0'] === 'string') {
                            text = value['0'];
                        }
                    }
                } finally {
                    reader.releaseLock();
                }

                return text;
            };

            declareAPI('LLMMain', async (id:string, promptStr:string, useMultimodal: boolean = false, optionsStr: string = '') => {
                let prompt:{
                    role: string,
                    content: string
                }[] = JSON.parse(promptStr)
                if(!ScriptingLowLevelIds.has(id)){
                    return
                }
                let promptbody:OpenAIChat[] = prompt.map((dict) => {
                    let role:'system'|'user'|'assistant' = 'assistant'
                    switch(dict['role']){
                        case 'system':
                        case 'sys':
                            role = 'system'
                            break
                        case 'user':
                            role = 'user'
                            break
                        case 'assistant':
                        case 'bot':
                        case 'char':{
                            role = 'assistant'
                            break
                        }
                    }

                    return {
                        content: dict['content'] ?? '',
                        role: role,
                    }
                })

                if(useMultimodal) {
                    for(const msg of promptbody) {
                        const inlays:string[] = []
                        msg.content = msg.content.replace(/{{(inlay|inlayed|inlayeddata)::(.+?)}}/g, (
                            match: string,
                            p1: string,
                            p2: string
                        ) => {
                            if(msg.role === 'assistant') {
                                if(p2 && p1 === 'inlayeddata') {
                                    inlays.push(p2)
                                }
                            }
                            else {
                                if(p2) {
                                    inlays.push(p2)
                                }
                            }
                            return ''
                        })
                        
                        const multimodals: MultiModal[] = []
                        for(const inlay of inlays) {
                            const inlayData = await getInlayAsset(inlay)
                            multimodals.push({
                                type: inlayData?.type,
                                base64: inlayData?.data,
                                width: inlayData?.width,
                                height: inlayData?.height
                            })
                        }

                        msg.multimodals = multimodals.length > 0 ? multimodals : undefined
                    }
                }

                const options = parseLuaOptions(optionsStr) as { streaming?: boolean }
                const result = await requestChatData({
                    formated: promptbody,
                    bias: {},
                    useStreaming: options.streaming === true,
                    forceStreaming: options.streaming === true,
                    noMultiGen: true,
                }, 'model')

                if(result.type === 'fail'){
                    return JSON.stringify({
                        success: false,
                        result: 'Error: ' + result.result
                    })
                }

                if(result.type === 'streaming'){
                    try {
                        return JSON.stringify({
                            success: true,
                            result: await collectLuaStreamText(result.result)
                        })
                    } catch (error) {
                        return JSON.stringify({
                            success: false,
                            result: 'Error: ' + error
                        })
                    }
                }

                if(result.type === 'multiline'){
                    return JSON.stringify({
                        success: false,
                        result: result.result
                    })
                }

                return JSON.stringify({
                    success: true,
                    result: result.result
                })
            })

            declareAPI('simpleLLM', async (id:string, prompt:string) => {
                if(!ScriptingLowLevelIds.has(id)){
                    return
                }
                const result = await requestChatData({
                    formated: [{
                        role: 'user',
                        content: prompt
                    }],
                    bias: {},
                    useStreaming: false,
                    noMultiGen: true,
                }, 'model')

                if(result.type === 'fail'){
                    return {
                        success: false,
                        result: 'Error: ' + result.result
                    }
                }

                if(result.type === 'streaming' || result.type === 'multiline'){
                    return {
                        success: false,
                        result: result.result
                    }
                }

                return {
                    success: true,
                    result: result.result
                }
            })
            
            declareAPI('getName', (id:string) => {
                const db = getScriptingDatabase()
                const selectedChar = getScriptingCharacterIndex(db)
                const char = db.characters[selectedChar]
                return char.name
            })

            declareAPI('setName', (id:string, name:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                if(typeof name !== 'string'){
                    throw('Invalid data type')
                }
                updateCapturedCharacterField('name', name)
            })

            declareAPI('getDescription', (id:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                const selectedChar = getScriptingCharacterIndex(DBState.db)
                const char = DBState.db.characters[selectedChar]

                return char.desc
            })

            declareAPI('setDescription', (id:string, desc:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                if(typeof desc !== 'string'){
                    throw('Invalid data type')
                }

                updateCapturedCharacterField('desc', desc)
            })

            declareAPI('getCharacterFirstMessage', (id:string) => {
                const selectedChar = getScriptingCharacterIndex(DBState.db)
                const char = DBState.db.characters[selectedChar]
                return char.firstMessage
            })

            declareAPI('setCharacterFirstMessage', (id:string, data:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                if(typeof data !== 'string'){
                    return false
                }
                return updateCapturedCharacterField('firstMessage', data)
            })

            declareAPI('getPersonaName', (id:string) => {
                return getUserName()
            })

            declareAPI('getPersonaDescription', (id:string) => {
                const db = getScriptingDatabase()
                const selectedChar = getScriptingCharacterIndex(db)
                const char = db.characters[selectedChar]

                return risuChatParser(getPersonaPrompt(), { chara: char })
            })

            declareAPI('getAuthorsNote', (id:string) => {
                return readScriptingChat()?.note ?? ''
            })

            declareAPI('getBackgroundEmbedding', (id:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                const db = getScriptingDatabase()
                const selectedChar = getScriptingCharacterIndex(db)
                const char = db.characters[selectedChar]
                return char.backgroundHTML
            })

            declareAPI('setBackgroundEmbedding', (id:string, data:string) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }
                if(typeof data !== 'string'){
                    return false
                }
                return updateCapturedCharacterField('backgroundHTML', data)
            })

            // Lore books
            declareAPI('getLoreBooksMain', (id:string, search:string) => {
                const db = getScriptingDatabase()
                const selectedChar = db.characters[getScriptingCharacterIndex(db)]


                const loreSources = [
                    selectedChar.chats[selectedChar.chatPage]?.localLore ?? [],
                    selectedChar.globalLore,
                    getModuleLorebooks()
                ]

                const found = []
                for (const source of loreSources) {
                    for (const b of source) {
                        if (b.comment === search) {
                            found.push({ ...b, content: risuChatParser(b.content, { chara: selectedChar }) })
                        }
                    }
                }

                return JSON.stringify(found)
            })

            type upsertLoreBookOptions = {
                alwaysActive?: boolean
                insertOrder?: number
                key?: string
                secondKey?: string
                regex?: boolean
            }

            declareAPI('upsertLocalLoreBook', (id:string, name:string, content:string, options:upsertLoreBookOptions) => {
                if(!ScriptingSafeIds.has(id)){
                    return
                }

                if (!ScriptingEngineState.operationCharacter) {
                    return
                }

                const currentChat = writeScriptingChat()

                const {
                    alwaysActive = false,
                    insertOrder = 100,
                    key = '',
                    regex = false,
                    secondKey = '',
                } = options

                if (!currentChat) {
                    return
                }

                const newLocalLoreBooks = (currentChat.localLore ?? [])
                    .filter((book) => book.comment !== name)
                newLocalLoreBooks.push({
                    alwaysActive,
                    comment: name,
                    content: content,
                    insertorder: insertOrder,
                    mode: 'normal',
                    key,
                    secondkey: secondKey,
                    selective: !!secondKey,
                    useRegex: regex,
                })
                currentChat.localLore = newLocalLoreBooks
            })

            declareAPI('loadLoreBooksMain', async (id:string, reserve:number) => {
                if(!ScriptingLowLevelIds.has(id)){
                    return
                }

                const db = getScriptingDatabase()

                const selectedChar = db.characters[getScriptingCharacterIndex(db)]



                const fullLoreBooks = (await loadLoreBookV3PromptFromCompatibilitySnapshot()).actives
                const maxContext = db.maxContext - reserve
                if (maxContext < 0) {
                    return JSON.stringify([])
                }

                let totalTokens = 0
                const loreBooks = []

                for (const book of fullLoreBooks) {
                    const parsed = risuChatParser(book.prompt, { chara: selectedChar }).trim()
                    if (parsed.length === 0) {
                        continue
                    }

                    const tokens = await tokenize(parsed)

                    if (totalTokens + tokens > maxContext) {
                        break
                    }
                    totalTokens += tokens
                    loreBooks.push({
                        data: parsed,
                        role: book.role === 'assistant' ? 'char' : book.role,
                    })
                }

                return JSON.stringify(loreBooks)
            })

            declareAPI('axLLMMain', async (id:string, promptStr:string, useMultimodal: boolean = false, optionsStr: string = '') => {
                let prompt:{
                    role: string,
                    content: string
                }[] = JSON.parse(promptStr)
                if(!ScriptingLowLevelIds.has(id)){
                    return
                }
                let promptbody:OpenAIChat[] = prompt.map((dict) => {
                    let role:'system'|'user'|'assistant' = 'assistant'
                    switch(dict['role']){
                        case 'system':
                        case 'sys':
                            role = 'system'
                            break
                        case 'user':
                            role = 'user'
                            break
                        case 'assistant':
                        case 'bot':
                        case 'char':{
                            role = 'assistant'
                            break
                        }
                    }

                    return {
                        content: dict['content'] ?? '',
                        role: role,
                    }
                })

                if(useMultimodal) {
                    for(const msg of promptbody) {
                        const inlays:string[] = []
                        msg.content = msg.content.replace(/{{(inlay|inlayed|inlayeddata)::(.+?)}}/g, (
                            match: string,
                            p1: string,
                            p2: string
                        ) => {
                            if(msg.role === 'assistant') {
                                if(p2 && p1 === 'inlayeddata') {
                                    inlays.push(p2)
                                }
                            }
                            else {
                                if(p2) {
                                    inlays.push(p2)
                                }
                            }
                            return ''
                        })
                        
                        const multimodals: MultiModal[] = []
                        for(const inlay of inlays) {
                            const inlayData = await getInlayAsset(inlay)
                            multimodals.push({
                                type: inlayData?.type,
                                base64: inlayData?.data,
                                width: inlayData?.width,
                                height: inlayData?.height
                            })
                        }

                        msg.multimodals = multimodals.length > 0 ? multimodals : undefined
                    }
                }

                const options = parseLuaOptions(optionsStr) as { streaming?: boolean }
                const result = await requestChatData({
                    formated: promptbody,
                    bias: {},
                    useStreaming: options.streaming === true,
                    forceStreaming: options.streaming === true,
                    noMultiGen: true,
                }, 'otherAx')

                if(result.type === 'fail'){
                    return JSON.stringify({
                        success: false,
                        result: 'Error: ' + result.result
                    })
                }

                if(result.type === 'streaming'){
                    try {
                        return JSON.stringify({
                            success: true,
                            result: await collectLuaStreamText(result.result)
                        })
                    } catch (error) {
                        return JSON.stringify({
                            success: false,
                            result: 'Error: ' + error
                        })
                    }
                }

                if(result.type === 'multiline'){
                    return JSON.stringify({
                        success: false,
                        result: result.result
                    })
                }

                return JSON.stringify({
                    success: true,
                    result: result.result
                })
            })

            declareAPI('getCharacterLastMessage', (id: string) => {
                const chat = readScriptingChat()
                if (!chat) {
                    return ''
                }

                const selchar = getScriptingCharacter()

                let pointer = chat.message.length - 1
                while (pointer >= 0) {
                    if (chat.message[pointer].role === 'char') {
                        const messageData = chat.message[pointer].data
                        return messageData
                    }
                    pointer--
                }

                return selchar?.firstMessage ?? ''
            })

            declareAPI('getUserLastMessage', (id: string) => {
                const chat = readScriptingChat()
                if (!chat) {
                    return ''
                }

                let pointer = chat.message.length - 1
                while (pointer >= 0) {
                    if (chat.message[pointer].role === 'user') {
                        const messageData = chat.message[pointer].data
                        return messageData
                    }
                    pointer--
                }

                return ''
            })

            declareAPI('getCharacterLastMessage', (id: string) => {
                const chat = readScriptingChat()
                if (!chat) {
                    return ''
                }

                const selchar = getScriptingCharacter()

                let pointer = chat.message.length - 1
                while (pointer >= 0) {
                    if (chat.message[pointer].role === 'char') {
                        const messageData = chat.message[pointer].data
                        return messageData
                    }
                    pointer--
                }

                return selchar?.firstMessage ?? ''
            })

            declareAPI('getUserLastMessage', (id: string) => {
                const chat = readScriptingChat()
                if (!chat) {
                    return ''
                }

                let pointer = chat.message.length - 1
                while (pointer >= 0) {
                    if (chat.message[pointer].role === 'user') {
                        const messageData = chat.message[pointer].data
                        return messageData
                    }
                    pointer--
                }
                return ''
            })

            console.log('Running Lua code:', code)
            if(ScriptingEngineState.type === 'lua'){
                await runLuaSource(ScriptingEngineState.engine, luaCodeWrapper(code))
            }
            if(ScriptingEngineState.type === 'py'){
                await ScriptingEngineState.pyodide?.init(code)
            }
            ScriptingEngineState.code = code
            } catch (error) {
                discardFailedScriptingEngineState(ScriptingEngineState)
                throw error
            }
        }
        let accessKey = v4()
        if(mode === 'editDisplay'){
            ScriptingEditDisplayIds.add(accessKey)
        }
        else{
            ScriptingSafeIds.add(accessKey)
            if(lowLevelAccess){
                ScriptingLowLevelIds.add(accessKey)
            }
        }
        try {
        let res:any
        if(ScriptingEngineState.type === 'lua'){
            const luaEngine = ScriptingEngineState.engine
            const listenerStartedAt = performance.now()
            try {
                switch(mode){
                    case 'input':{
                        const func = luaEngine.global.get('onInput')
                        if(func){
                            res = await func(accessKey)
                        }
                        break
                    }
                    case 'output':{
                        const func = luaEngine.global.get('onOutput')
                        if(func){
                            res = await func(accessKey)
                        }
                        break
                    }
                    case 'start':{
                        const func = luaEngine.global.get('onStart')
                        if(func){
                            res = await func(accessKey)
                        }
                        break
                    }
                    case 'onButtonClick':{
                        const func = luaEngine.global.get('onButtonClick')
                        if(func){
                            res = await func(accessKey, data)
                        }
                        break
                    }
                    case 'editRequest':
                    case 'editDisplay':
                    case 'editInput':
                    case 'editOutput':{
                        const func = luaEngine.global.get('callListenMain')
                        if(func){
                            res = await func(mode, accessKey, JSON.stringify(data), JSON.stringify(meta))
                            res = JSON.parse(res)
                        }
                        break
                    }
                    default:{
                        const func = luaEngine.global.get(mode)
                        if(func){
                            res = await func(accessKey)
                        }
                        break
                    }
                }   
                if(res === false){
                    ScriptingEngineState.stopSending = true
                }
            } catch (error) {
                // Wasmoon wraps synchronous hook timeouts in a plain Error with this first line.
                if ((luaTimeoutErrorConstructor && error instanceof luaTimeoutErrorConstructor) ||
                    (error instanceof Error && error.message.split('\n', 1)[0] === 'Error: thread timeout exceeded')) {
                    const diagnosticMode = ['input', 'output', 'start', 'onButtonClick', 'editRequest',
                        'editDisplay', 'editInput', 'editOutput', 'manual'].includes(mode) ? mode : 'custom'
                    console.error({ code: 'lua-listener-timeout', mode: diagnosticMode,
                        elapsedMs: performance.now() - listenerStartedAt })
                    noticeScriptTimeout(mode === 'editDisplay')
                } else {
                    console.error(error)
                }
            }
        }
        if(ScriptingEngineState.type === 'py'){
            switch(mode){
                case 'input':{
                    res = await ScriptingEngineState.pyodide?.python(`onInput('${accessKey}')`)
                    break
                }
                case 'output':{
                    res = await ScriptingEngineState.pyodide?.python(`onOutput('${accessKey}')`)
                    break
                }
                case 'start':{
                    res = await ScriptingEngineState.pyodide?.python(`onStart('${accessKey}')`)
                    break
                }
                case 'onButtonClick':{
                    res = await ScriptingEngineState.pyodide?.python(`onButtonClick('${accessKey}', '${data as string}')`)
                    break
                }
                case 'editRequest':
                case 'editDisplay':
                case 'editInput':
                case 'editOutput':{
                    res = await ScriptingEngineState.pyodide?.python(`callListenMain('${mode}', '${accessKey}', '${JSON.stringify(data)}', '${JSON.stringify(meta)}')`)
                    res = JSON.parse(res)
                    break
                }
                default:{
                    res = await ScriptingEngineState.pyodide?.python(`${mode}('${accessKey}')`)
                    break
                }
            }
        }
        chat = access?.currentChat() ?? ScriptingEngineState.chat

        return {
            stopSending: ScriptingEngineState.stopSending ?? false, chat, res
        }
        } finally {
            ScriptingSafeIds.delete(accessKey)
            ScriptingLowLevelIds.delete(accessKey)
            ScriptingEditDisplayIds.delete(accessKey)
            invocationFinished = true
            finishScriptingEngineUse(ScriptingEngineState)
        }
        } finally {
            clearScriptingEngineInvocationState(ScriptingEngineState)
        }
    })
    } finally {
        if (!invocationFinished) {
            finishScriptingEngineUse(ScriptingEngineState)
        }
    }
}

async function makeLuaFactory(){
    const wasmoon = await import('wasmoon')
    luaTimeoutErrorConstructor = wasmoon.LuaTimeoutError
    const _luaFactory = await createLuaFactory()
    async function mountFile(name:string){
        let code = ''
        for(let i = 0; i < 3; i++){
            try {
                const res = await fetch('/lua/' + name)
                if(res.status >= 200 && res.status < 300){
                    code = await res.text()
                    break
                }
            } catch (error) {}
        }
        await _luaFactory.mountFile(name,code)
    }

    await mountFile('json.lua')
    luaFactory = _luaFactory
}

async function ensureLuaFactory() {
    if (luaFactory) return;

    const pendingFactory = (luaFactoryPromise ??= makeLuaFactory())
    try {
        await pendingFactory
    } finally {
        if (luaFactoryPromise === pendingFactory) {
            luaFactoryPromise = null
        }
    }
}

function getOrCreateEngineState(
    mode: string, 
    type: 'lua'|'py',
    ownerChaId: string,
    code: string,
    functionTimeout: number,
): ScriptingEngineState {
    const cacheKey = JSON.stringify([type, ownerChaId, mode, code, functionTimeout])
    let engineState = ScriptingEngines.get(cacheKey);
    if (engineState) {
        engineState.lastUsed = ++scriptingEngineRecency
        return engineState;
    }

    engineState = {
        mutex: new Mutex(),
        cacheKey,
        activeUses: 0,
        lastUsed: ++scriptingEngineRecency,
        type,
    };
    ScriptingEngines.set(cacheKey, engineState);
    return engineState;
}

function evictIdleScriptingEngines() {
    const maxEntries = getRuntimePerformanceBudgets().scriptingEngineCacheEntries
    while (ScriptingEngines.size > maxEntries) {
        let oldest: ScriptingEngineState | undefined
        for (const state of ScriptingEngines.values()) {
            if (state.activeUses === 0 && (!oldest || state.lastUsed < oldest.lastUsed)) {
                oldest = state
            }
        }
        if (!oldest) {
            return
        }

        ScriptingEngines.delete(oldest.cacheKey)
        if (oldest.type === 'lua') {
            oldest.engine?.global.close()
            oldest.engine = undefined
        } else {
            oldest.pyodide?.close()
            oldest.pyodide = undefined
        }
    }
}

function finishScriptingEngineUse(state: ScriptingEngineState) {
    state.activeUses--
    state.lastUsed = ++scriptingEngineRecency
    if (state.code === undefined) {
        removeScriptingEngineStateIfUnused(state)
    }
    evictIdleScriptingEngines()
}

function discardFailedScriptingEngineState(state: ScriptingEngineState) {
    state.code = undefined
    if (state.type === 'lua') {
        state.engine?.global.close()
        state.engine = undefined
    } else {
        state.pyodide?.close()
        state.pyodide = undefined
    }
    removeScriptingEngineStateIfUnused(state)
}

function removeScriptingEngineStateIfUnused(state: ScriptingEngineState) {
    if (state.activeUses === 0 && ScriptingEngines.get(state.cacheKey) === state) {
        ScriptingEngines.delete(state.cacheKey)
    }
}

function luaCodeWrapper(code:string){
    return `
json = require 'json'

function getChat(id, index)
    return json.decode(getChatMain(id, index))
end

function getFullChat(id)
    return json.decode(getFullChatMain(id))
end

function getRecentChats(id, count)
    return json.decode(getRecentChatsMain(id, count))
end

function setFullChat(id, value)
    setFullChatMain(id, json.encode(value))
end

function log(value)
    logMain(json.encode(value))
end

function getLoreBooks(id, search)
    return json.decode(getLoreBooksMain(id, search))
end


function loadLoreBooks(id)
    return json.decode(loadLoreBooksMain(id):await())
end

function LLM(id, prompt, useMultimodal, options)
    useMultimodal = useMultimodal or false
    options = options or {}
    return json.decode(LLMMain(id, json.encode(prompt), useMultimodal, json.encode(options)):await())
end

function axLLM(id, prompt, useMultimodal, options)
    useMultimodal = useMultimodal or false
    options = options or {}
    return json.decode(axLLMMain(id, json.encode(prompt), useMultimodal, json.encode(options)):await())
end

function getCharacterImage(id)
    return getCharacterImageMain(id):await()
end

function getPersonaImage(id)
    return getPersonaImageMain(id):await()
end

local editRequestFuncs = {}
local editDisplayFuncs = {}
local editInputFuncs = {}
local editOutputFuncs = {}

function listenEdit(type, func)
    if type == 'editRequest' then
        editRequestFuncs[#editRequestFuncs + 1] = func
        return
    end

    if type == 'editDisplay' then
        editDisplayFuncs[#editDisplayFuncs + 1] = func
        return
    end

    if type == 'editInput' then
        editInputFuncs[#editInputFuncs + 1] = func
        return
    end

    if type == 'editOutput' then
        editOutputFuncs[#editOutputFuncs + 1] = func
        return
    end

    throw('Invalid type')
end

function getState(id, name)
    local escapedName = "__"..name
    return json.decode(getChatVar(id, escapedName))
end

function setState(id, name, value)
    local escapedName = "__"..name
    setChatVar(id, escapedName, json.encode(value))
end

function setStateChanged(id, name, value)
    local escapedName = "__"..name
    return setChatVarChanged(id, escapedName, json.encode(value))
end

function async(callback)
    return function(...)
        local co = coroutine.create(callback)
        local safe, result = coroutine.resume(co, ...)

        return Promise.create(function(resolve, reject)
            local checkresult
            local step = function()
                if coroutine.status(co) == "dead" then
                    local send = safe and resolve or reject
                    return send(result)
                end

                safe, result = coroutine.resume(co)
                checkresult()
            end

            checkresult = function()
                if safe and result == Promise.resolve(result) then
                    result:finally(step):catch(reject)
                else
                    step()
                end
            end

            checkresult()
        end)
    end
end

callListenMain = async(function(type, id, value, meta)
    local realValue = json.decode(value)
    local realMeta = json.decode(meta)

    if type == 'editRequest' then
        for _, func in ipairs(editRequestFuncs) do
            realValue = func(id, realValue, realMeta)
        end
    end

    if type == 'editDisplay' then
        for _, func in ipairs(editDisplayFuncs) do
            realValue = func(id, realValue, realMeta)
        end
    end

    if type == 'editInput' then
        for _, func in ipairs(editInputFuncs) do
            realValue = func(id, realValue, realMeta)
        end
    end

    if type == 'editOutput' then
        for _, func in ipairs(editOutputFuncs) do
            realValue = func(id, realValue, realMeta)
        end
    end

    return json.encode(realValue)
end)

${code}
`
}

export async function runLuaEditTrigger<T extends string | OpenAIChat[]>(
    char: character | simpleCharacterArgument,
    mode: string,
    content: T,
    meta?: object,
    conversationOperation?: ConversationOperationContext,
    onConversationCommit?: ConversationCommitObserver,
    signal?: AbortSignal,
): Promise<T> {
    const run = () => runLuaEditTriggerForMode(
        char, mode, content, meta, conversationOperation, onConversationCommit, signal,
    )
    if (mode !== 'editdisplay' && mode !== 'editDisplay') return run()
    const endDisplayRun = beginDisplayScriptRun()
    try {
        return await run()
    } finally {
        endDisplayRun()
    }
}

async function runLuaEditTriggerForMode<T extends string | OpenAIChat[]>(
    char: character | simpleCharacterArgument,
    mode: string,
    content: T,
    meta?: object,
    conversationOperation?: ConversationOperationContext,
    onConversationCommit?: ConversationCommitObserver,
    signal?: AbortSignal,
): Promise<T> {
    switch (mode) {
        case 'editinput':
            mode = 'editInput'
            break
        case 'editoutput':
            mode = 'editOutput'
            break
        case 'editdisplay':
            mode = 'editDisplay'
            break
        case 'editprocess':
            return content
    }

    if (mode === 'editDisplay' && !conversationOperation) {
        const hasLuaTrigger = (trigger: triggerscript) =>
            trigger?.effect?.[0]?.type === 'triggerlua'
        if (
            !(char.triggerscript?.some(hasLuaTrigger)) &&
            !getModuleTriggers().some(hasLuaTrigger)
        ) {
            return content
        }
        const session = peekActiveConversationSession()
        const sourceChat = getCurrentChat()
        if (
            session?.isActive &&
            sourceChat &&
            session.materializeCompatibilityArray() === sourceChat.message
        ) {
            let mutex = luaDisplayOperationMutexes.get(session)
            if (!mutex) {
                mutex = new Mutex()
                luaDisplayOperationMutexes.set(session, mutex)
            }
            // Display listeners can write variables. Capture their operation after the
            // preceding display commits, so concurrent rows do not share a stale baseline.
            return mutex.runExclusive(async () => {
                if (
                    peekActiveConversationSession() !== session ||
                    !session.isActive ||
                    getCurrentChat() !== sourceChat ||
                    session.materializeCompatibilityArray() !== sourceChat.message
                ) {
                    return content
                }
                return runLuaEditTriggerBatch(
                    char,
                    mode,
                    content,
                    meta,
                    undefined,
                    onConversationCommit,
                    signal,
                )
            })
        }
    }
    return runLuaEditTriggerBatch(
        char,
        mode,
        content,
        meta,
        conversationOperation,
        onConversationCommit,
        signal,
    )
}

async function runLuaEditTriggerBatch<T extends string | OpenAIChat[]>(
    char: character | simpleCharacterArgument,
    mode: string,
    content: T,
    meta?: object,
    conversationOperation?: ConversationOperationContext,
    onConversationCommit?: ConversationCommitObserver,
    signal?: AbortSignal,
): Promise<T> {
    let operationContext = conversationOperation
    let historyWindowCopy: HistoryWindowCopy | null = null
    let access: ScriptConversationAccess | null = null
    try {
        let data = content

        const triggers =
            char.triggerscript
                      .map<triggerscript>((v) => ({ ...v, lowLevelAccess: false }))
                      .concat(getModuleTriggers())
        if (!triggers.some((trigger) => trigger?.effect?.[0]?.type === 'triggerlua')) {
            return content
        }
        // A send that builds from a history window hands its edit listeners the window.
        const historyWindow = !operationContext && mode !== 'editDisplay'
            ? findActiveHistoryWindow(char.chaId, getCurrentChat()?.id)
            : null
        const sourceCurrent = historyWindow?.isCurrent()
        const sourceVersion = historyWindow?.mutationVersion
        const sourceMessages = historyWindow ? [...historyWindow.chat.message] : null
        const createWindowChat = historyWindow ? () => {
            if (!historyWindowCopy) {
                if (!sourceCurrent || !historyWindow.isCurrent()
                    || sourceVersion !== historyWindow.mutationVersion
                    || sourceMessages!.length !== historyWindow.chat.message.length
                    || sourceMessages!.some((message, index) => historyWindow.chat.message[index] !== message)) {
                    throw new PersistentMutationFencedError()
                }
                historyWindowCopy = openHistoryWindowCopy(historyWindow, onConversationCommit)
            }
            return historyWindowCopy.chat
        } : undefined
        if (!operationContext && !historyWindow) {
            access = createScriptConversationAccess(getCurrentChat(), getDatabase(), {
                onCommitted: onConversationCommit,
                ...(mode === 'editDisplay'
                    ? { origin: 'display' as const }
                    : { onVariablesCommitted: (keys) => requestScriptDisplayRefresh({ keys }) }),
            })
        }

        for (let trigger of triggers) {
            signal?.throwIfAborted()
            if (trigger?.effect?.[0]?.type === 'triggerlua') {
                const runResult = await runScripted(trigger.effect[0].code, {
                    char: char,
                    lowLevelAccess: false,
                    mode: mode,
                    data,
                    meta,
                    operationContext,
                    access: access ?? undefined,
                    chat: historyWindow?.chat,
                    createConversationChat: createWindowChat,
                })
                data = runResult.res ?? data
            }
        }

        return data
    } catch (error) {
        return content
    } finally {
        if (historyWindowCopy && !historyWindowCopy.commit()) {
            throw new PersistentMutationFencedError()
        }
        // A listener that changed nothing keeps its result after the conversation closes.
        access?.commitIfChanged()
    }
}

export async function runLuaButtonTrigger(
    char: character | simpleCharacterArgument,
    data: string,
    conversationOperation?: ConversationOperationContext,
): Promise<any> {
    if (conversationOperation) {
        return runLuaButtonTriggerBatch(char, data, conversationOperation)
    }
    return runSerializedUserTrigger(char.chaId, getCurrentChat(), () =>
        runLuaButtonTriggerBatch(char, data),
    )
}

async function runLuaButtonTriggerBatch(
    char: character|simpleCharacterArgument,
    data: string,
    conversationOperation?: ConversationOperationContext,
): Promise<any>{
    let runResult
    let access: ScriptConversationAccess | null = null
    try {
        const triggers = char.triggerscript.map<triggerscript>((v) => ({
            ...v,
            lowLevelAccess: char.type !== 'simple' ? char.lowLevelAccess ?? false : false
        })).concat(getModuleTriggers())
        if (!triggers.some((trigger) => trigger?.effect?.[0]?.type === 'triggerlua')) {
            return undefined
        }
        if (!conversationOperation) {
            access = createScriptConversationAccess(getCurrentChat(), getDatabase(), {
                onVariablesCommitted: (keys) => requestScriptDisplayRefresh({ keys }),
            })
        }

        for(let trigger of triggers){
            if(trigger?.effect?.[0]?.type === 'triggerlua'){
                runResult = await runScripted(trigger.effect[0].code, {
                    char: char,
                    lowLevelAccess: trigger.lowLevelAccess,
                    mode: 'onButtonClick',
                    data: data,
                    operationContext: conversationOperation,
                    access: access ?? undefined,
                    userStarted: true,
                })
            }
        }
    } catch (error) {
        throw(error)
    } finally {
        if (access) {
            access.commit()
            if (runResult) runResult.chat = access.liveChat
        }
    }
    return runResult   
}

class PyodideContext{
    worker: Worker;
    apis: Record<string, (...args:any[]) => any> = {};
    inited: boolean = false;
    constructor(){
        this.worker = new Worker(new URL('./pyworker.ts', import.meta.url), {
            type: 'module'
        })
        this.worker.onmessage = (event:MessageEvent) => {
            if(event.data.type === 'call'){
                const { function: func, args, callId } = event.data;
                if(this.apis[func]){
                    this.apis[func](...args).then((result) => {
                        this.worker.postMessage({
                            type: 'functionResult',
                            callId: callId,
                            result: result
                        });
                    }).catch((error) => {
                        this.worker.postMessage({
                            type: 'error',
                            error: error.message,
                            id: callId
                        });
                    });
                } else {
                    this.worker.postMessage({
                        type: 'error',
                        error: `Function ${func} not found`,
                        id: callId
                    });
                }
            }
        }
    }
    declareAPI(name:string, func:(...args:any[]) => any){
        this.apis[name] = func;
    }
    async init(code:string){
        if(this.inited){
            return;
        }
        const id = crypto.randomUUID();
        return new Promise<void>((resolve, reject) => {
            this.worker.onmessage = (event:MessageEvent) => {
                if(event.data.id !== id){
                    return
                }

                if(event.data.type === 'init'){
                    this.inited = true;
                    resolve();
                } else if(event.data.type === 'error'){
                    reject(new Error(event.data.error));
                }
            };
            this.worker.postMessage({
                type: 'init',
                code: code,
                id: id,
                moduleFunctions: Object.keys(this.apis)
            });
        });
    }
    async python(call:string){
        const id = crypto.randomUUID();
        return new Promise<any>((resolve, reject) => {
            this.worker.onmessage = (event:MessageEvent) => {
                if(event.data.id !== id){
                    return
                }

                if(event.data.type === 'python'){
                    resolve(event.data.call);
                } else if(event.data.type === 'error'){
                    reject(new Error(event.data.error));
                }
            };
            this.worker.postMessage({
                type: 'python',
                call: call,
                id: id
            });
        });
    }
    close(){
        this.worker.terminate();
    }
}
