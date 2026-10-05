import { offerHtmlClipboardExport } from './htmlClipboardExport'
import { defaultChatToggleBinding } from './toggleBindings'
import { get, writable } from "svelte/store";
import { saveImage, type character, type Chat, defaultSdDataFunc, type loreBook, getDatabase, getCharacterByIndex, setCharacterByIndex } from "./storage/database.svelte";
import { alertAddCharacter, alertCheckboxConfirm, alertClear, alertError, alertNormal, alertSelect, alertStore, alertToast, alertWait } from "./alert";
import { language } from "../lang";
import { isArchivedCharacter } from "./storage/workingSetCatalog";
import { restoreArchivedCharacterWithConfirmation } from "./storage/characterArchive";
import { checkNullish, findCharacterbyId, getUserName, selectMultipleFile, selectSingleFile } from "./util";
import { v4 as uuidv4, v4 } from 'uuid';
import { getImageType } from "./media";
import { DBState, MobileGUIStack, OpenRealmStore, selectedCharID } from "./stores.svelte";
import { AppendableBuffer, changeChatTo, checkCharOrder, createChatCopyName, downloadFile, getFileSrc } from "./globalApi.svelte";
import { updateInlayScreen } from "./process/inlayScreen";
import { parseMarkdownSafe } from "./parser/parser.svelte";
import { translateHTML } from "./translator/translator";
import { doingChat } from "./process/index.svelte";
import { importCharacter } from "./characterCards";
import { PngChunk } from "./pngChunk";
import {
    acquireCompleteConversation,
    activateCharacter,
    captureSelectedConversationTarget,
    commitCharacterAddition,
    deactivateActiveWorkingSet,
    deletePersistentCharacterWithGroupReferences,
    editWindowedChatList,
    fencePersistentNavigation,
    flushPendingData,
    getPersistentNavigationGeneration,
    getSelectedConversationMode,
    mutatePersistentCharacterDetail,
    readPersistentCompleteCharacter,
    readPersistentConversation,
    reconcilePersistentActiveCharacterIds,
} from "./storage/persistentDataRuntime.svelte";
import type { groupChat } from "./storage/database.svelte";
import { removeCharacterIdFromOrder } from './storage/characterOrderMutation'
import { safeStructuredClone } from './polyfill'
import { isConversationSummaryStub } from './storage/conversationResidency'
import { isMetadataOnlySelectedConversation } from './storage/selectedConversationLifecycle'
import { open } from '@tauri-apps/plugin-dialog'
import { readFile } from '@tauri-apps/plugin-fs'
import { isTauriDesktop } from './platform'
import {
    archiveCurrentCharacterImage,
    selectCharacterImageFile,
} from './storage/characterImageFileRoute'
import { importNativeJpegAsset } from './storage/nativeJpegAssetImport'
import { SelectedConversationPromotionStaleError } from './storage/activeWorkingSet.svelte'
import { PersistentMutationFencedError } from './storage/saveCoordinator'
import { beginNavigationActivity } from './ui/navigationActivity'
import { yieldToUi } from './ui/yieldToUi'

export async function commitDetachedCharacter(
    character: character | groupChat,
    reason: string,
): Promise<string> {
    const characterId = character.chaId
    await commitCharacterAddition({
        characterId,
        estimatedBytes: new TextEncoder().encode(JSON.stringify(character)).byteLength,
        install() {
            DBState.db.characters.push(character)
            checkCharOrder()
        },
    }, reason)
    return characterId
}

export async function createNewCharacter(): Promise<string> {
    return commitDetachedCharacter(createBlankChar(), 'create-character')
}

export async function createNewGroup(): Promise<string> {
    const character: groupChat = {
        type: 'group',
        name: "",
        firstMessage: "",
        chats: [{
            ...defaultChatToggleBinding(DBState.db),
            message: [],
            note: '',
            name: 'Chat 1',
            localLore: [],
            id: v4()
        }],
        chatFolders: [],
        chatPage: 0,
        viewScreen: 'none',
        globalLore: [],
        characters: [],
        autoMode: false,
        useCharacterLore: true,
        emotionImages: [],
        customscript: [],
        chaId: uuidv4(),
        firstMsgIndex: -1,
        characterTalks: [],
        characterActive: [],
        realmId: ''
    }
    return commitDetachedCharacter(character, 'create-group')
}

export async function getCharImage(loc:string, type:'plain'|'css'|'contain'|'lgcss') {
    const db = DBState.db
    
    // Return placeholder when hideAllImages is enabled
    if(db.hideAllImages){
        if(type === 'plain'){
            return '/none.webp'
        }
        return ''  // For CSS types, return empty to show default ? icon
    }
    
    if(!loc || loc === ''){
        if(type ==='css'){
            return ''
        }
        return null
    }
    const filesrc = await getFileSrc(loc)
    if(type === 'plain'){
        return filesrc
    }
    else if(type ==='css'){
        return `background: url("${filesrc}");background-size: cover;`
    }
    else if(type === 'lgcss'){
        return `background: url("${filesrc}");background-size: cover;height: 10.66rem;`

    }

    else{
        return `background: url("${filesrc}");background-size: contain;background-repeat: no-repeat;background-position: center;`
    }
}

export async function selectCharImg(charIndex:number) {
    const characterId = DBState.db.characters[charIndex]?.chaId
    if (!characterId) return
    await selectCharacterImageFile(characterId, {
        isTauriDesktop,
        chooseDesktopPath: async () => {
            const selected = await open({
                multiple: false,
                directory: false,
                filters: [{
                    name: 'png, webp, gif, jpg, jpeg',
                    extensions: ['png', 'webp', 'gif', 'jpg', 'jpeg'],
                }],
            })
            return typeof selected === 'string' ? selected : null
        },
        readDesktopPath: readFile,
        chooseLegacyFile: async () => selectSingleFile(['png', 'webp', 'gif', 'jpg', 'jpeg']),
        nativeImport: importNativeJpegAsset,
        legacyImport: async (img) => {
            const type = getImageType(img)
            const pngExif: Record<string, string> = {}
            try {
                if(type === 'PNG'){
                    const gen = PngChunk.readGenerator(img)
                    const allowedChunk = [
                        'parameters', 'Comment', 'Title', 'Description', 'Author', 'Software', 'Source', 'Disclaimer', 'Warning', 'Copyright',
                    ]
                    for await (const chunk of gen){
                        if(chunk instanceof AppendableBuffer){
                            continue
                        }
                        if(!chunk){
                            continue
                        }
                        if(chunk.value.length > 20_000){
                            continue
                        }
                        if(allowedChunk.includes(chunk.key)){
                            pngExif[chunk.key] = chunk.value
                        }
                    }
                }
            } catch (error) {
                console.error(error)
            }
            const imgp = await saveImage(img)
            await mutatePersistentCharacterDetail(characterId, 'select-character-image', ({ character }) => {
                archiveCurrentCharacterImage(character)
                character.image = imgp
                if (character.type === 'character' && Object.keys(pngExif).length > 0) {
                    character.extentions ??= {}
                    character.extentions.pngExif = { ...character.extentions.pngExif, ...pngExif }
                }
            })
            return imgp
        },
    })
}

export function dumpCharImage(charIndex:number) {
    const char = DBState.db.characters[charIndex] as character
    if(!char.image || char.image === ''){
        return
    }
    archiveCurrentCharacterImage(char)
    DBState.db.characters[charIndex] = char
}

export function changeCharImage(charIndex:number,changeIndex:number) {
    const char = DBState.db.characters[charIndex] as character
    const image = char.ccAssets[changeIndex].uri
    char.ccAssets.splice(changeIndex, 1)
    dumpCharImage(charIndex)
    char.image = image
    DBState.db.characters[charIndex] = char
}


export const addingEmotion = writable(false)

export async function addCharEmotion(charId:number) {
    const characterId = DBState.db.characters[charId]?.chaId
    if (!characterId || get(addingEmotion)) return
    addingEmotion.set(true)
    try {
        const selected = await selectMultipleFile(['png', 'webp', 'gif'])
        if (!selected?.length) return
        const images: [string, string][] = []
        for (const f of selected) {
            const imgp = await saveImage(f.data)
            images.push([f.name.replace('.png', '').replace('.webp', ''), imgp])
        }
        await mutatePersistentCharacterDetail(characterId, 'add-character-emotions', ({ character }) => {
            if (character.type === 'character') character.emotionImages.push(...images)
        })
    } finally {
        addingEmotion.set(false)
    }
}

export function rmCharEmotion(charId:number, emotionId:number) {
    let dbChar = DBState.db.characters[charId]
    if(dbChar.type !== 'group'){
        dbChar.emotionImages.splice(emotionId, 1)
        DBState.db.characters[charId] = dbChar
    }
}


export async function exportChat(page:number){
    try {

        const mode = await alertSelect(['Export as JSON', "Export as TXT", "Export as HTML File", "Export as HTML Embed"])
        const doTranslate = (mode === '2' || mode === '3') ? (await alertSelect([language.translateContent, language.doNotTranslate])) === '0' : false
        const anonymous = (mode === '2' || mode === '3') ? ((await alertSelect([language.includePersonaName, language.hidePersonaName])) === '1') : false
        const selectedID = get(selectedCharID)
        const db = DBState.db
        const char = db.characters[selectedID]
        const chatId = char.chats[page]?.id
        if (!chatId) throw new Error('Chat was not found')
        const chat = await readPersistentConversation(char.chaId, chatId, 'export-chat')
        if (!chat) throw new Error('Chat was not found')
        const date = new Date().toJSON();
        const htmlChatParse = async (v:string) => {
            v = parseMarkdownSafe(v)

            if(doTranslate){
                v = await translateHTML(v, false, '', -1)
            }

            if(anonymous){
                //case insensitive match, replace all
                const excapedName = char.name.replace(/[-\/\\^$*+\?\.()|[\]{}]/g, '\\$&')

                v = v.replace(new RegExp(`${excapedName}`, 'gi'), '×××')
            }

            return v
        }

        if(mode === '0'){
            let folders = []
            if(chat.folderId) {
                folders = db.characters[selectedID].chatFolders?.filter(f => f.id === chat.folderId)
            }
            const stringl = Buffer.from(JSON.stringify({
                type: 'risuChat',
                ver: 2,
                data: chat,
                folders: folders
            }), 'utf-8')
    
            await downloadFile(`${char.name}_${date}_chat`.replace(/[<>:"/\\|?*\.\,]/g, "") + '.json', stringl)
    
        }
        else if(mode === '2'){

            let chatContentHTML = ''

            let i = 0
            for(const v of chat.message){
                alertWait(`Translating... ${i++}/${chat.message.length}`)
                const name = v.saying ? findCharacterbyId(v.saying).name : v.role === 'char' ? char.name : anonymous ? '×××' : getUserName()
                chatContentHTML += `<div class="chat">
                    <h2>${name}</h2>
                    <div>${await htmlChatParse(v.data)}</div>
                </div>`
            }

            const doc = `
                <!DOCTYPE html>
                <html>
                    <head>
                        <title>${char.name} Chat</title>
                        <style>
                            body{
                                font-family: Arial, sans-serif;
                                display: flex;
                                justify-content: center;
                            }
                            .container{
                                max-width: 800px;
                                padding: 1rem;
                                border-radius: 10px;
                                display: flex;
                                flex-direction: column;
                                gap: 1rem;
                            }
                            .chat{
                                background: #f0f0f0;
                                padding: 1rem;
                                border-radius: 10px;
                                display: flex;
                                flex-direction: column;
                            }
                            .idat{
                                display: none;
                            }
                            h2{
                                margin: 0;
                            }
                            .chat div{
                                margin-top: 0.5rem;
                                break-word: break-all;
                            }
                        </style>
                    </head>
                    <body>
                        <div class="container">
                            <div class="chat">
                                <h2>${char.name}</h2>
                                <div>${await htmlChatParse(
                                    chat.fmIndex === -1 ? char.firstMessage : char.alternateGreetings?.[chat.fmIndex ?? 0]
                                )}</div>
                            </div>
                            ${chatContentHTML}
                        </div>
                        <div class="idat">${
                            JSON.stringify(chat).replace(/</g, '&lt;').replace(/>/g, '&gt;')
                        }</div>
                    </body>
            `


            await downloadFile(`${char.name}_${date}_chat`.replace(/[<>:"/\\|?*\.\,]/g, "") + '.html', Buffer.from(doc, 'utf-8'))
        }
        else if(mode === '3'){
            //create a html table
            let chatContentHTML = ''

            let i = 0
            for(const v of chat.message){
                alertWait(`Translating... ${i++}/${chat.message.length}`)
                const name = v.saying ? findCharacterbyId(v.saying).name : v.role === 'char' ? char.name : anonymous ? '×××' : getUserName()
                chatContentHTML += `<tr>
                    <td>${name}</td>
                    <td>${await htmlChatParse(v.data)}</td>
                </tr>`
            }

            const template = `
                <table>
                    <tr>
                        <th>Character</th>
                        <th>Message</th>
                    </tr>
                    <tr>
                        <td>${char.name}</td>
                        <td>${await htmlChatParse(char.firstMessage)}</td>
                    </tr>
                    ${chatContentHTML}
                </table>
                <p>Chat from RisuNest</p>
            `

            offerHtmlClipboardExport(template, `${char.name}_${date}_chat`.replace(/[<>:"/\\|?*\.\,]/g, "") + '.html', {
                present: value => alertStore.set(value),
                download: downloadFile,
                success: copied => alertNormal(copied ? language.clipboardSuccess : language.successExport),
                error: error => alertError(String(error)),
                labels: { copy: language.copy, download: language.download, cancel: language.cancel },
            })
            return

        }
        else{
            
            let stringl = chat.message.map((v) => {
                if(v.saying){
                    return `--${findCharacterbyId(v.saying).name}\n${v.data}`
                }
                else{
                    return `--${v.role === 'char' ? char.name : getUserName()}\n${v.data}`
                }
            }).join('\n\n')

            if(char.type !== 'group'){
                stringl = `--${char.name}\n${char.firstMessage}\n\n` + stringl
            }

            await downloadFile(`${char.name}_${date}_chat`.replace(/[<>:"/\\|?*\.\,]/g, "") + '.txt', Buffer.from(stringl, 'utf-8'))

        }
        alertNormal(language.successExport)
    } catch (error) {
        alertError(error)
    }
}

export async function importChat(){
    const dat =await selectSingleFile(['json','jsonl','txt','html'])
    if(!dat){
        return
    }
    try {
        const selectedID = get(selectedCharID)
        const characterId = DBState.db.characters[selectedID].chaId

        if(dat.name.endsWith('jsonl')){
            const lines = Buffer.from(dat.data).toString('utf-8').split('\n')
            let newChat:Chat = {
                message: [],
                note: "",
                name: "Imported Chat",
                localLore: [],
                fmIndex: -1,
                id: v4()
            }

            let isFirst = true
            for(const line of lines){
                
                const presedLine = JSON.parse(line)
                if(presedLine.name && presedLine.is_user, presedLine.mes){
                    if(!isFirst){
                        newChat.message.push({
                            role: presedLine.is_user ? "user" : 'char',
                            data: formatTavernChat(presedLine.mes, DBState.db.characters[selectedID].name),
                            chatId: v4(),
                        })
                    }
                }

                isFirst = false
            }

            if(newChat.message.length === 0){
                alertError(language.errors.noData)
                return
            }

            const imported = await editSelectedChatList(characterId, 'import-chat', (character) => {
                if(character.chatFolders
                    .filter(folder => folder.id === newChat.folderId).length === 0) {
                    newChat.folderId = null
                }
                character.chats.unshift(newChat)
                return newChat.id
            })
            if(imported){
                alertNormal(language.successImport)
            }
        }
        else if(dat.name.endsWith('json')){
            // The Buffer polyfill collects every code unit in an array before decoding,
            // which needs several times the file size; a large chat file runs the renderer out of memory.
            const json = JSON.parse(new TextDecoder('utf-8', { ignoreBOM: true }).decode(dat.data))
            if((json.type === 'risuAllChats' || json.type === 'risuChat') && json.ver === 2){
                const folders = json.folders || []
                const chats = Array.isArray(json.data) ? json.data : [json.data]
                const imported = await editSelectedChatList(characterId, 'import-chat', (character) => {
                    let folderIdMap = {}
                    folders.forEach(folder => {
                        if(character.chatFolders?.some(f => f.id === folder.id)){
                            const newId = uuidv4()
                            folderIdMap[folder.id] = newId
                            folder.id = newId
                        } else {
                            folderIdMap[folder.id] = folder.id
                        }
                    })
                    if(character.chatFolders === undefined){
                        character.chatFolders = []
                    }
                    character.chatFolders.push(...folders)
                    chats.forEach(chat => {
                        if(chat.folderId && folderIdMap[chat.folderId]){
                            chat.folderId = folderIdMap[chat.folderId]
                        }
                        chat.id = v4()
                    })
                    character.chats.unshift(...chats)
                    return null
                })
                if(imported){
                    alertNormal(language.successImport)
                }
                return
            }
            if(json.type === 'risuAllChats' && json.ver === 1){
                const chats = json.data
                if(Array.isArray(chats) && chats.length > 0){
                    const imported = await editSelectedChatList(characterId, 'import-chat', (character) => {
                        const usedIds = new Set(character.chats.map((chat) => chat.id))
                        character.chats.unshift(...(chats.map((v) => {
                            if(!v.id || usedIds.has(v.id)){
                                v.id = uuidv4()
                            }
                            usedIds.add(v.id)
                            if(!v.localLore){
                                v.localLore = []
                            }
                            v.fmIndex ??= -1
                            return v
                        })))
                        return null
                    })
                    if(imported){
                        alertNormal(language.successImport)
                    }
                    return
                } else {
                    alertError(language.errors.noData)
                    return
                }
            }
            if(json.type === 'risuChat' && json.ver === 1){
                const das:Chat = json.data
                if(!(checkNullish(das.message) || checkNullish(das.note) || checkNullish(das.name) || checkNullish(das.localLore))){
                    das.fmIndex ??= -1
                    das.id = v4()
                    const imported = await editSelectedChatList(characterId, 'import-chat', (character) => {
                        character.chats.unshift(das)
                        return null
                    })
                    if(imported){
                        alertNormal(language.successImport)
                    }
                    return
                }
                else{
                    alertError(language.errors.noData)
                    return   
                }
            }
            else{
                alertError(language.errors.noData)
                return
            }
        }
        else if(dat.name.endsWith('html')){
            const doc = new DOMParser().parseFromString(Buffer.from(dat.data).toString('utf-8'), 'text/html')
            const chat = doc.querySelector('.idat').textContent
            const json = JSON.parse(chat)
            if(json.message && json.note && json.name && json.localLore){
                json.id = v4()
                const imported = await editSelectedChatList(characterId, 'import-chat', (character) => {
                    character.chats.unshift(json)
                    return null
                })
                if(imported){
                    alertNormal(language.successImport)
                }
            }
            else{
                alertError(language.errors.noData)
            }
        }
    } catch (error) {
        alertError(error)
    }
}

export async function exportAllChats() {
    try {
        const selectedID = get(selectedCharID)
        const db = getDatabase()
        const selected = db.characters[selectedID]
        const char = selected
            ? await readPersistentCompleteCharacter(selected.chaId, 'export-all-chats')
            : null
        if (!char) throw new Error('Character was not found')
        const date = new Date().toISOString().replace(/[:.]/g, "-")
        const allChats = char.chats
        const allFolders = char.chatFolders
        const stringl = Buffer.from(JSON.stringify({
            type: 'risuAllChats',
            ver: 2,
            data: allChats,
            folders: allFolders
        }), 'utf-8')
        await downloadFile(`${char.name}_all_chats_${date}`.replace(/[<>:"/\\|?*.,]/g, "") + '.json', stringl)
        alertNormal(language.successExport)
    } catch (error) {
        alertError(error)
    }
}

function formatTavernChat(chat:string, charName:string){
    const db = getDatabase()
    return chat.replace(/<([Uu]ser)>|\{\{([Uu]ser)\}\}/g, getUserName()).replace(/((\{\{)|<)([Cc]har)(=.+)?((\}\})|>)/g, charName)
}

export function characterFormatUpdate(indexOrCharacter:number|character|groupChat, arg:{
    updateInteraction?:boolean,
} = {}){
    let cha = typeof(indexOrCharacter) === 'number' ? getCharacterByIndex(indexOrCharacter) : indexOrCharacter
    if(cha.chats.length === 0){
        cha.chats = [{
            message: [],
            note: '',
            name: 'Chat 1',
            localLore: [],
            id: v4()
        }]
    }
    if(!cha.chats[cha.chatPage]){
        cha.chatPage = 0
    }
    if(
        !isMetadataOnlySelectedConversation(cha.chats[cha.chatPage]) &&
        !cha.chats[cha.chatPage].message
    ){
        cha.chats[cha.chatPage].message = []
    }
    if(!cha.type){
        cha.type = 'character'
    }
    if(!cha.chaId){
        cha.chaId = uuidv4()
    }
    if(cha.type !== 'group'){
        if(checkNullish(cha.sdData)){
            cha.sdData = defaultSdDataFunc()
        }
        if(checkNullish(cha.utilityBot)){
            cha.utilityBot = false
        }
        cha.triggerscript = cha.triggerscript ?? []
        cha.alternateGreetings = cha.alternateGreetings ?? []
        cha.exampleMessage = cha.exampleMessage ?? ''
        cha.creatorNotes = cha.creatorNotes ?? ''
        cha.systemPrompt = cha.systemPrompt ?? ''
        cha.tags = cha.tags ?? []
        cha.creator = cha.creator ?? ''
        cha.characterVersion = cha.characterVersion ?? ''
        cha.personality = cha.personality ?? ''
        cha.scenario = cha.scenario ?? ''
        cha.firstMsgIndex = cha.firstMsgIndex ?? -1
        cha.additionalData = cha.additionalData ?? {
            tag: [],
            creator: '',
            character_version: ''
        }
        cha.voicevoxConfig = cha.voicevoxConfig ?? {
            SPEED_SCALE: 1,
            PITCH_SCALE: 0,
            INTONATION_SCALE: 1,
            VOLUME_SCALE: 1
        }
        if(cha.postHistoryInstructions){
            cha.chats[cha.chatPage].note += "\n" + cha.postHistoryInstructions
            cha.chats[cha.chatPage].note = cha.chats[cha.chatPage].note.trim()
            cha.postHistoryInstructions = null
        }
        cha.additionalText ??= ''
        cha.depth_prompt ??= {
            depth: 0,
            prompt: ''
        }
        cha.hfTTS ??= {
            model: '',
            language: 'en'
        }
        cha.backgroundHTML ??= ''
        cha.backgroundCSS ??= ''
        cha.creation_date ??= Date.now()
        cha.globalLore = updateLorebooks(cha.globalLore)
        if(!cha.newGenData){
            cha = updateInlayScreen(cha)
        }
        // Migrate legacy 'none' value to '' for UI dropdown compatibility
        // Using '' because it's falsy, so `if (ttsMode)` correctly detects enabled TTS
        if (cha.ttsMode === 'none') {
            cha.ttsMode = ''
        }
        cha.ttsMode ??= ''
    }
    else{
        if((!cha.characterTalks) || cha.characterTalks.length !== cha.characters.length){
            cha.characterTalks = []
            for(let i=0;i<cha.characters.length;i++){
                cha.characterTalks.push(1 / 6 * 4)
            }
        }
        if((!cha.characterActive) || cha.characterActive.length !== cha.characters.length){
            cha.characterActive = []
            for(let i=0;i<cha.characters.length;i++){
                cha.characterActive.push(true)
            }
        }
    }
    if(checkNullish(cha.customscript)){
        cha.customscript = []
    }
    cha.lastInteraction = Date.now()
    if(typeof(indexOrCharacter) === 'number'){
        setCharacterByIndex(indexOrCharacter, cha)
    }
    for(let i = 0; i < cha.chats.length; i++){
        const chat = cha.chats[i]
        if(isConversationSummaryStub(chat)) continue
        chat.fmIndex ??= cha.firstMsgIndex ?? -1
        if(!chat.id){
            chat.id = uuidv4()
        }
        if(!chat.localLore){
            chat.localLore = []
        }
    }
    return cha
}

export function updateLorebooks(book:loreBook[]){
    return book.map((v) => {
        v.bookVersion ??= 1
        if(v.bookVersion >= 2){
            return v
        }
        if(v.activationPercent){
            const perc = v.activationPercent
            v.activationPercent = null

            v.content = `@@probability ${perc}\n${v.content}`
        }
        v.content = v.content.replace(/@@@?end/g, '@@depth 0').replace(/\<(char|bot)\>/g, '{{char}}').replace(/\<(user)\>/g, '{{user}}')
        v.bookVersion = 2
        return v
    })

}

export function createBlankChar():character{
    return {
        name: '',
        firstMessage: '',
        desc: '',
        notes: '',
        chats: [{
            ...defaultChatToggleBinding(DBState.db),
            message: [],
            note: '',
            name: 'Chat 1',
            localLore: [],
            id: v4()
        }],
        chatFolders: [],
        chatPage: 0,
        emotionImages: [],
        bias: [],
        viewScreen: 'none',
        globalLore: [],
        chaId: uuidv4(),
        type: 'character',
        sdData: defaultSdDataFunc(),
        utilityBot: false,
        customscript: [],
        exampleMessage: '',
        creatorNotes:'',
        systemPrompt:'',
        postHistoryInstructions:'',
        alternateGreetings:[],
        tags:[],
        creator:"",
        characterVersion: '',
        personality:"",
        scenario:"",
        firstMsgIndex: -1,
        replaceGlobalNote: "",
        triggerscript: [{
            comment: "",
            type: "manual",
            conditions: [],
            effect: [{
                type: "v2Header",
                code: "",
                indent: 0
            }]
        }, {
            comment: "New Event",
            type: 'manual',
            conditions: [],
            effect: []
        }],
        additionalText: ''
    }
}


export async function makeGroupImage() {
    try {
        alertStore.set({
            type: 'wait',
            msg: `Loading..`
        })
        const db = getDatabase()
        const charID = get(selectedCharID)
        const group = db.characters[charID]
        if(group.type !== 'group'){
            return
        }
    
        const imageUrls = await Promise.all(group.characters.map((v) => {
            return getCharImage(findCharacterbyId(v).image, 'plain')
        }))
    
        
    
        const canvas = document.createElement("canvas");
        canvas.width = 256
        canvas.height = 256
        const ctx = canvas.getContext("2d");
      
        // Load the images
        const images = [];
        let loadedImages = 0;
      
        await Promise.all(
            imageUrls.map(
            (url) =>
                new Promise<void>((resolve) => {
                    const img = new Image();
                    img.crossOrigin="anonymous"
                    img.onload = () => {
                        images.push(img);
                        resolve();
                    };
                    img.src = url;
                })
            )
        );
      
        // Calculate dimensions and draw the grid
        const numImages = images.length;
        const numCols = Math.ceil(Math.sqrt(images.length));
        const numRows = Math.ceil(images.length / numCols);
        const cellWidth = canvas.width / numCols;
        const cellHeight = canvas.height / numRows;
      
        for (let row = 0; row < numRows; row++) {
          for (let col = 0; col < numCols; col++) {
            const index = row * numCols + col;
            if (index >= numImages) break;
            ctx.drawImage(
              images[index],
              col * cellWidth,
              row * cellHeight,
              cellWidth,
              cellHeight
            );
          }
        }
      
        // Return the image URI
    
        const uri = canvas.toDataURL()
        canvas.remove()
        db.characters[charID].image = await saveImage(dataURLtoBuffer(uri));
        alertClear()
    } catch (error) {
        alertError(error)
    }
}

function dataURLtoBuffer(string:string){
    const regex = /^data:.+\/(.+);base64,(.*)$/;

    const matches = string.match(regex);
    const ext = matches[1];
    const data = matches[2];
    return Buffer.from(data, 'base64');
}

export async function removeChar(identifier:string|number,name:string, type:'normal'|'permanent'|'permanentForce' = 'normal'){
    const liveDatabase = getDatabase()
    const targetId = typeof identifier === 'string'
        ? identifier
        : liveDatabase.characters[identifier]?.chaId
    if (!targetId) return
    if(type !== 'permanentForce'){
        if (!(await alertCheckboxConfirm({
            title: language.removeConfirm + name,
            description: type === 'normal'
                ? language.checkboxConfirmation.characterTrashDescription
                : language.checkboxConfirmation.characterDeletionDescription,
            checkboxLabel: language.checkboxConfirmation.characterDeletion,
            actionLabel: language.confirm,
            cancelLabel: language.cancel,
            requireChecked: true,
        })).confirmed) return
    }
    const selected = liveDatabase.characters[get(selectedCharID)]
    const targetTrashTime = liveDatabase.characters.find(
        (character) => character.chaId === targetId,
    )?.trashTime
    const selectedCharacterId = selected?.chaId ?? null
    if (!await deactivateActiveWorkingSet()) return
    const deactivatedGeneration = getPersistentNavigationGeneration()
    const restoreSelectionAfterFailedMutation = async () => {
        const currentSelectedId = DBState.db.characters[get(selectedCharID)]?.chaId ?? null
        if (
            !selectedCharacterId ||
            currentSelectedId !== selectedCharacterId ||
            getPersistentNavigationGeneration() !== deactivatedGeneration
        ) return
        const beforeActivationGeneration = getPersistentNavigationGeneration()
        let restored = false
        try {
            restored = await activateCharacter(selectedCharacterId)
        } catch {
            restored = false
        }
        if (restored) return
        const afterActivationGeneration = getPersistentNavigationGeneration()
        const selectedAfterFailure = DBState.db.characters[get(selectedCharID)]?.chaId ?? null
        if (
            selectedAfterFailure === selectedCharacterId &&
            (
                afterActivationGeneration === beforeActivationGeneration ||
                afterActivationGeneration === beforeActivationGeneration + 1
            )
        ) {
            selectedCharID.set(-1)
            reconcilePersistentActiveCharacterIds(DBState.db, null)
        }
    }
    const clearSelectionAfterCommittedReplacement = async () => {
        const currentSelectedId = DBState.db.characters[get(selectedCharID)]?.chaId ?? null
        if (
            getPersistentNavigationGeneration() !== deactivatedGeneration ||
            (currentSelectedId !== selectedCharacterId && currentSelectedId !== null)
        ) return
        if (!await deactivateActiveWorkingSet()) return
        selectedCharID.set(-1)
        reconcilePersistentActiveCharacterIds(DBState.db, null)
    }
    const clearSelectionAfterCommittedDetail = () => {
        const currentSelectedId = DBState.db.characters[get(selectedCharID)]?.chaId ?? null
        if (
            getPersistentNavigationGeneration() !== deactivatedGeneration ||
            (currentSelectedId !== selectedCharacterId && currentSelectedId !== null)
        ) return
        selectedCharID.set(-1)
        reconcilePersistentActiveCharacterIds(DBState.db, null)
    }
    let changed = false
    try {
        if (type === 'normal') {
            changed = await mutatePersistentCharacterDetail(
                targetId,
                'character-removal',
                ({ root, character }) => {
                    removeCharacterIdFromOrder(root, targetId)
                    character.trashTime = Date.now()
                },
            )
        } else {
            changed = await deletePersistentCharacterWithGroupReferences(
                targetId,
                'character-removal',
            )
        }
    } catch (error) {
        const locallyCommitted = type !== 'normal' && !DBState.db.characters.some(
            (character) => character.chaId === targetId,
        )
        const locallyTrashed = type === 'normal' && DBState.db.characters.some(
            (character) => (
                character.chaId === targetId &&
                character.trashTime !== undefined &&
                character.trashTime !== targetTrashTime
            ),
        )
        if (locallyCommitted) {
            await clearSelectionAfterCommittedReplacement().catch(() => false)
        } else if (locallyTrashed) {
            clearSelectionAfterCommittedDetail()
        } else {
            await restoreSelectionAfterFailedMutation().catch(() => false)
        }
        throw error
    }
    if (!changed) {
        await restoreSelectionAfterFailedMutation()
        return
    }
    if (type !== 'normal') {
        await clearSelectionAfterCommittedReplacement()
    } else {
        clearSelectionAfterCommittedDetail()
    }
}

export async function addCharacter(arg:{
    reseter?:()=>any,
} = {}){
    const reseter = arg.reseter ?? (() => {})
    MobileGUIStack.set(100)
    let finalStack = 1
    try {
        const r = await alertAddCharacter()
        if(r === 'importFromRealm'){
            if (!await deactivateActiveWorkingSet()) return
            selectedCharID.set(-1)
            OpenRealmStore.set(true)
            finalStack = 0
            return
        }
        reseter();
        let addedCharacterId: string | null = null
        switch(r){
            case 'createfromScratch':
                addedCharacterId = await createNewCharacter()
                break
            case 'createGroup':
                addedCharacterId = await createNewGroup()
                break
            case 'importCharacter':
                addedCharacterId = await importCharacter()
                break
            default:
                return
        }
        if(addedCharacterId){
            const currentIndex = getDatabase().characters.findIndex(
                (character) => character.chaId === addedCharacterId,
            )
            await changeChar(currentIndex)
        }
    } catch (error) {
        alertError(error)
    } finally {
        MobileGUIStack.set(finalStack)
    }
}

export async function changeChar(index: number, arg:{
    reseter?:()=>any,
} = {}): Promise<boolean> {
    const reseter = arg.reseter ?? (() => {})
    const target = DBState.db.characters?.[index]
    const chaId = target?.chaId
    // Reactivating the open character would reload it and reset the chat view.
    if(
        chaId &&
        get(selectedCharID) === index &&
        captureSelectedConversationTarget()?.characterId === chaId
    ){
        reseter()
        return true
    }
    if(get(doingChat)){
      alertToast(language.navigationBlockedWhileGenerating)
      return false
    }
    if(!chaId) return false
    if(isArchivedCharacter(target)){
        await restoreArchivedCharacterWithConfirmation(chaId)
        return false
    }
    const restoreNavigationGeneration = fencePersistentNavigation()
    const activity = beginNavigationActivity('character')
    const isRestoreCurrent = () =>
        activity.isCurrent() &&
        getPersistentNavigationGeneration() === restoreNavigationGeneration
    try {
        reseter()
        await yieldToUi()
        if (!isRestoreCurrent()) return false
        if (!isRestoreCurrent()) return false
        const expectedNavigationGeneration = restoreNavigationGeneration + 1
        const activationOptions = {
            normalize: (candidate: character | groupChat) =>
                characterFormatUpdate(candidate, {
                    updateInteraction: true,
                }),
        }
        let activated = await activateCharacter(chaId, activationOptions)
        if (!activated) {
            if (getPersistentNavigationGeneration() !== expectedNavigationGeneration) return false
            activated = await activateCharacter(chaId, activationOptions)
        }
        if(!activated) return false
        if (!activity.isCurrent()) return false
        return true
    } catch (error) {
        alertError(error)
        return false
    } finally {
        activity.finish()
    }
}

/**
 * Edits the selected character's chat list. A windowed selection records the
 * edit as evidence against the saved list; otherwise the selected conversation
 * is held complete, so neither path persists an unexplained list change. The
 * previously selected chat stays selected unless `edit` returns the id of the
 * chat to switch to. `edit` returns false to abandon the change. It may run
 * twice, first on a draft and again on the complete path when the draft edit
 * cannot be recorded.
 */
export async function editSelectedChatList(
    characterId: string,
    reason: string,
    edit: (character: character | groupChat) => string | null | false,
): Promise<boolean> {
    const blockedByGeneration = () => {
        if (!get(doingChat)) return false
        alertToast(language.navigationBlockedWhileGenerating)
        return true
    }
    const resolveCharacter = () => {
        const current = DBState.db.characters[get(selectedCharID)]
        return current?.chaId === characterId ? current : null
    }
    if (!resolveCharacter() || blockedByGeneration()) return false
    const windowed = await editWindowedSelectedChatList(characterId, reason, resolveCharacter, edit)
    if (windowed !== 'unsupported') return windowed
    if (blockedByGeneration()) return false
    const target = captureSelectedConversationTarget()
    if (target && target.characterId !== characterId) return false
    let lease: Awaited<ReturnType<typeof acquireCompleteConversation>> | null = null
    if (target) {
        try {
            lease = await acquireCompleteConversation(reason, target)
        } catch (error) {
            if (error instanceof SelectedConversationPromotionStaleError) return false
            throw error
        }
    }
    try {
        if (blockedByGeneration()) return false
        const character = resolveCharacter()
        if (!character) return false
        const selectedId = character.chats[character.chatPage]?.id
        if (lease && selectedId !== target?.conversationId) return false
        const nextId = edit(character)
        if (nextId === false) return false
        const selectedIndex = character.chats.findIndex((chat) => chat.id === selectedId)
        if (selectedIndex !== -1 && character.chatPage !== selectedIndex) {
            character.chatPage = selectedIndex
        }
        if (lease) {
            try {
                await flushPendingData(reason)
            } catch (error) {
                // The edit stays in the working set and is saved once storage accepts input.
                if (error instanceof PersistentMutationFencedError) return false
                throw error
            }
        }
        return nextId === null ? true : await changeChatTo(nextId)
    } finally {
        lease?.release()
    }
}

async function editWindowedSelectedChatList(
    characterId: string,
    reason: string,
    resolveCharacter: () => character | groupChat | null,
    edit: (character: character | groupChat) => string | null | false,
): Promise<boolean | 'unsupported'> {
    const initial = captureSelectedConversationTarget()
    if (
        !initial ||
        initial.characterId !== characterId ||
        getSelectedConversationMode() !== 'windowed'
    ) return 'unsupported'
    const flush = async () => {
        try {
            await flushPendingData(reason)
            return true
        } catch (error) {
            // The edit stays in the working set and is saved once storage accepts input.
            if (error instanceof PersistentMutationFencedError) return false
            throw error
        }
    }
    // The edit is described against the saved list, so earlier changes go first.
    if (!(await flush())) return false
    const target = captureSelectedConversationTarget()
    if (
        !target ||
        target.characterId !== characterId ||
        target.conversationId !== initial.conversationId ||
        !resolveCharacter()
    ) return false
    const result = editWindowedChatList(target, edit)
    if (result.kind === 'unsupported') return 'unsupported'
    if (result.kind === 'refused') return false
    if (!(await flush())) return false
    return result.nextId === null ? true : await changeChatTo(result.nextId)
}

export async function addNewChat(character: character | groupChat): Promise<boolean> {
    return editSelectedChatList(character.chaId, 'add-chat', (current) => {
        const chats = current.chats
        const newChat: Chat = {
            ...defaultChatToggleBinding(DBState.db),
            message: [],
            note: '',
            name: `New Chat ${chats.length + 1}`,
            localLore: [],
            fmIndex: -1,
            id: uuidv4(),
        }
        if(current.type === 'group'){
            for(const memberId of current.characters){
                newChat.message.push({
                    saying: memberId,
                    role: 'char',
                    data: findCharacterbyId(memberId).firstMessage,
                    chatId: v4(),
                })
            }
        }
        chats.unshift(newChat)
        current.chats = chats
        return newChat.id
    })
}

export async function duplicateChat(characterId: string, chatId: string): Promise<boolean> {
    const selectedBeforeRead = getDatabase().characters[get(selectedCharID)]?.chaId
    if (selectedBeforeRead !== characterId) return false
    const source = await readPersistentConversation(characterId, chatId, 'duplicate-chat')
    if (!source) return false
    return editSelectedChatList(characterId, 'duplicate-chat', (character) => {
        if (!character.chats.some((conversation) => conversation.id === chatId)) return false
        const duplicate = safeStructuredClone(source)
        duplicate.name = createChatCopyName(duplicate.name, 'Copy')
        duplicate.id = v4()
        character.chats.unshift(duplicate)
        character.chats = character.chats
        return duplicate.id
    })
}

export async function removeChat(character: character | groupChat, chatId: string): Promise<boolean> {
    const chats = character.chats
    if(!chats.some((chat) => chat.id === chatId)) return false
    const selectedChatId = chats[character.chatPage]?.id
    if (selectedChatId === chatId) {
        // Leave the chat first so the removal never targets the selected conversation.
        const survivingId = chats.find((candidate) => candidate.id !== chatId)?.id
        if (!survivingId) return false
        if (!(await changeChatTo(survivingId)) && !(await changeChatTo(survivingId))) return false
    }
    return editSelectedChatList(character.chaId, 'remove-chat', (current) => {
        const removeIndex = current.chats.findIndex((chat) => chat.id === chatId)
        if (removeIndex === -1 || removeIndex === current.chatPage) return false
        current.chats.splice(removeIndex, 1)
        current.chats = current.chats
        return null
    })
}
