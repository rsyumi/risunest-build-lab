import type { Chat, Database, character, loreBook } from '../storage/database.svelte'
import type { RisuModule } from '../process/modules'

function lore(key: string): loreBook {
    return {
        key,
        secondkey: '',
        insertorder: 100,
        comment: key,
        content: `${key} content`,
        mode: 'normal',
        alwaysActive: false,
        selective: false,
    }
}

function module(id: string, namespace?: string): RisuModule {
    return {
        id,
        name: `Module ${id}`,
        description: '',
        lorebook: [lore(id)],
        regex: [],
        ...(namespace ? { namespace } : {}),
    }
}

function conversation(id: string, extra: Partial<Chat> = {}): Chat {
    return {
        id,
        name: `Chat ${id}`,
        note: `${id} note`,
        localLore: [lore(`${id}-local`)],
        message: [
            { role: 'user', data: `${id} first`, chatId: `${id}-m0`, __yumi_tr: { text: 'kept' }, __other: 'dropped' } as never,
            { role: 'char', data: `${id} second`, chatId: `${id}-m1` },
            { role: 'user', data: `${id} third`, chatId: `${id}-m2`, __yumi_tr: { text: 'third' } } as never,
        ],
        ...extra,
    }
}

function baseCharacter(id: string, name: string, chats: Chat[], extra: Partial<character> = {}): character {
    return {
        type: 'character',
        chaId: id,
        name,
        image: '',
        firstMessage: `Hello from ${name}`,
        desc: `${name} description`,
        notes: '',
        chats,
        chatFolders: [],
        chatPage: 0,
        viewScreen: 'none',
        bias: [],
        emotionImages: [],
        globalLore: [lore(`${id}-global`)],
        sdData: [],
        customscript: [],
        triggerscript: [],
        utilityBot: false,
        exampleMessage: '',
        creatorNotes: '',
        systemPrompt: '',
        postHistoryInstructions: '',
        replaceGlobalNote: '',
        additionalText: '',
        alternateGreetings: [`${name} greeting`],
        tags: [],
        creator: 'fixture',
        characterVersion: '1',
        personality: `${name} personality`,
        scenario: `${name} scenario`,
        firstMsgIndex: 0,
        lastInteraction: 100,
        ...extra,
    }
}

/**
 * A stored library whose preset-mirrored and persona-mirrored root fields are stale, so a
 * read that takes them from the root instead of the active preset and persona shows up.
 */
export function conversationContextDatabase(): Database {
    const plain = baseCharacter('char-plain', 'Plain', [
        conversation('conv-plain', {
            bindedPersona: 'persona-bound',
            modules: ['module-chat'],
            scriptstate: { $present: 'stored', $both: 'stored-both', $number: 3, $flag: true },
            savedToggleValues: { toggle_a: 'bound-a' },
            GLGlobalVariables: { shared: 'local', ignored: 'null', empty: '' },
        }),
        conversation('conv-second'),
    ], {
        modules: ['module-char'],
        defaultVariables: 'charvar=character\nboth=character',
        translatorNote: 'Plain translator note',
        nickname: 'Plainy',
        loreSettings: { tokenBudget: 100, scanDepth: 3, recursiveScanning: false },
    })
    const member = baseCharacter('char-member', 'Member', [conversation('conv-member')], {
        nickname: 'Mem',
    })
    return {
        apiType: 'fixture',
        formatversion: 4,
        username: 'Stale root user',
        personaPrompt: 'Stale root prompt',
        loreBookDepth: 7,
        moduleIntergration: 'module-unused',
        templateDefaultVariables: 'stale=root',
        customPromptTemplateToggle: 'stale',
        presetRegex: [],
        botPresets: [
            { id: 'preset-other', name: 'Other', mainPrompt: 'other', moduleIntergration: 'module-unused' },
            {
                id: 'preset-active',
                name: 'Active',
                mainPrompt: 'active',
                moduleIntergration: ' module-int , ns-shared',
                templateDefaultVariables: 'tmpl=template\nboth=template\ncharvar=template',
                customPromptTemplateToggle: 'toggle_a=Toggle A',
                regex: [{ comment: 'preset regex', in: 'a', out: 'b', type: 'editoutput', ableFlag: false }],
            },
        ],
        botPresetsId: 1,
        personas: [
            { id: 'persona-selected', name: 'Selected Persona', personaPrompt: 'selected prompt', icon: '' },
            {
                id: 'persona-bound',
                name: 'Bound Persona',
                personaPrompt: 'bound prompt',
                icon: '',
                embeddedModule: module('module-embedded'),
            },
        ],
        selectedPersona: 0,
        modules: [
            module('module-enabled'),
            module('module-chat'),
            module('module-char'),
            module('module-int'),
            module('module-ns', 'ns-shared'),
            module('module-unused'),
        ],
        enabledModules: ['module-enabled'],
        explicitGlobalChatVariables: { shared: 'explicit', toggle_a: 'global-a', toggle_b: 'global-b' },
        globalChatVariables: { shared: 'stale' },
        pluginCustomStorage: {},
        characters: [plain, member],
    } as unknown as Database
}

/** The same library as the host holds it in memory: mirrors derived from the active preset and persona. */
export function conversationContextHostDatabase(): Database {
    const database = conversationContextDatabase()
    const preset = database.botPresets[database.botPresetsId]
    database.moduleIntergration = preset.moduleIntergration ?? ''
    database.templateDefaultVariables = preset.templateDefaultVariables ?? ''
    database.customPromptTemplateToggle = preset.customPromptTemplateToggle ?? ''
    database.presetRegex = preset.regex ?? []
    const persona = database.personas[database.selectedPersona]
    database.username = persona.name
    database.personaPrompt = persona.personaPrompt
    database.globalChatVariables = { ...database.explicitGlobalChatVariables! }
    return database
}
