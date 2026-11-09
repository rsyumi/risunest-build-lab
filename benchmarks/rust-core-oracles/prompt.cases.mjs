// Writes the case files the prompt oracle (`prompt.oracle.test.ts`) reads: `send` and `settings` under
// tests/fixtures/rust-core/prompt. The cases are deterministic and synthetic. Run
// `node benchmarks/rust-core-oracles/prompt.cases.mjs` and then the oracle with RUST_CORE_ORACLE_WRITE=1 to refresh the
// expectations. A case's `decline` names the input the Rust machine declines before any host request, dropping the
// changes collected by then; the oracle still records what TS does with it.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultOut = resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/prompt')
const out = process.argv.slice(2).find(value => !value.startsWith('--')) ?? defaultOut
mkdirSync(out, { recursive: true })

const write = (name, value) => writeFileSync(`${out}/${name}.cases.json`, JSON.stringify(value, null, 2) + '\n')

function writeSend() {
    const PS = { assistantPrefill: '', postEndInnerFormat: '', sendChatAsSystem: false, sendName: false, utilOverride: false }
    const ps = (values = {}) => ({ ...PS, ...values })
    const msg = (role, data, chatId, values = {}) => ({ role, data, chatId, ...values })

    const base = {
        root: {
            formatversion: 3, botPresetsId: 0, selectedPersona: 0,
            personas: [
                { id: 'persona-a', name: 'Synthetic user', personaPrompt: 'Synthetic persona.', note: '', icon: '' },
                { id: 'persona-b', name: 'Bound user', personaPrompt: 'Bound persona.', note: '', icon: '' },
            ],
            requestRetrys: 0, useStreaming: false, additionalPrompt: 'Extra instruction.', descriptionPrefix: 'Description: ',
            jailbreakToggle: false, presetChain: '', plugins: [], modules: [], enabledModules: [], loadouts: [],
        },
        preset: {
            id: 'preset-a', name: 'Synthetic preset', aiModel: 'custom', currentPluginProvider: 'synthetic-provider',
            mainPrompt: 'Synthetic main.', jailbreak: 'Synthetic jailbreak.', globalNote: 'Synthetic global note.',
            maxContext: 4000, maxResponse: 64, promptTemplate: null, promptPreprocess: false,
            formatingOrder: ['main', 'description', 'personaPrompt', 'chats', 'lastChat', 'jailbreak', 'lorebook', 'globalNote', 'authorNote'],
        },
        character: {
            type: 'character', chaId: 'character', name: 'Synthetic character', chatPage: 0, desc: 'Synthetic description.',
            firstMessage: 'Synthetic greeting.', alternateGreetings: ['Alternate greeting.'], bias: [], globalLore: [],
            customscript: [], triggerscript: [], newGenData: {},
        },
        conversation: { id: 'conversation', name: 'Synthetic conversation', note: 'Synthetic author note.', fmIndex: -1, localLore: [] },
        messages: [
            msg('user', ' First question. ', 'message-a'), msg('char', 'First answer. ', 'message-b'),
            msg('user', ' Second question. ', 'message-c'),
        ],
    }

    const longText = 'one two three four five six seven eight nine ten'
    const longMessages = [msg('user', longText, 'long-a'), msg('char', longText, 'long-b'), msg('user', longText, 'long-c')]
    const mainPlain = { type: 'plain', type2: 'main', text: 'Template main.', role: 'system' }
    const globalPlain = { type: 'plain', type2: 'globalNote', text: 'Template global.', role: 'system' }
    const chatAll = { type: 'chat', rangeStart: 0, rangeEnd: 'end' }
    const rolesTemplate = [
        { type: 'plain', type2: 'main', text: 'Template main.', role: 'user' },
        { type: 'jailbreak', type2: 'normal', text: 'Template jailbreak.', role: 'bot' },
        { type: 'cot', type2: 'normal', text: 'Template cot.', role: 'system' },
        { type: 'persona', role2: 'user' },
        { type: 'description', role2: 'bot' },
        { type: 'authornote', role2: 'user' },
        chatAll,
        { type: 'postEverything' },
    ]
    const examples = '<START>\n{{char}}: Example greeting.\n{{user}}: Example reply: with colon\nexample continuation\n<START>\n<bot>: Bot prefixed.'
    // TS escapes these characters to private-use code points before the parser runs.
    const escaped = 'Brace x, paren y, angle z, colon  semicolon .'
    const chatml = '<|im_start|>system\nChatML system.<|im_end|>\n<|im_start|>user<|im_sep|>ChatML user.<|im_end|>\n'
        + '<|im_start|>assistant ChatML <Thoughts>hidden</Thoughts>assistant.<|im_end|>\n<|im_start|>other line'
    const chatmlSeparators = '<|im_start|>system<|im_sep|>Separated system.<|im_end|>\n'
        + '<|im_start|>assistant<|im_sep|> Separated assistant. <|im_end|>\n<|im_start|>user<|im_sep|>Separated user.<|im_end|>'
    const proxy = {
        aiModel: 'reverse_proxy', forceReplaceUrl: 'http://synthetic-proxy.invalid/v1/chat/completions',
        proxyKey: 'synthetic-key', customProxyRequestModel: 'synthetic-model',
    }
    const recheckFailure = {
        mainPrompt: 'A longer synthetic main prompt for recheck.', maxContext: 150, maxResponse: 0,
        formatingOrder: [...Array(12).fill('main'), 'lastChat'],
    }
    const depthMessages = [
        msg('char', '<Thoughts>first</Thoughts>One.', 'td-a'), msg('char', '<Thoughts>second</Thoughts>Two.', 'td-b'),
        msg('char', '<Thoughts>third</Thoughts>Three.', 'td-c'), msg('user', '<Thoughts>fourth</Thoughts>Four.', 'td-d')]
    const setvar = (name, value) => ({ type: 'setvar', operator: '=', var: name, value })
    const startTrigger = (...effect) => [{ comment: '', type: 'start', conditions: [], effect }]

    const cases = [
        { id: 'base-order' },
        { id: 'builtin-cl100k', tokenizer: 'cl100k_base' },
        { id: 'builtin-o200k', tokenizer: 'o200k_base' },
        { id: 'jailbreak-cot-preprocess', root: { jailbreakToggle: true, chainOfThought: true }, preset: { promptPreprocess: true } },
        { id: 'editprocess-hooks', hooks: [{ suffix: ' [first hook]' }, { keep: true }, { replace: ['question', 'query'] }] },
        { id: 'editprocess-nullish-empty', hooks: [{ nullish: true }, { empty: true }] },
        { id: 'context-limit-failure', preset: { maxContext: 20 } },
        { id: 'main-roles', preset: { mainPrompt: '@@system\n First system. \n@@@user\n Main user. \n@@assistant\nMain assistant.\n@@user\n' } },
        { id: 'main-no-marker-prefix', preset: { mainPrompt: 'Intro line.\n@@user\nUser part.' } },
        { id: 'main-marker-not-first', preset: { mainPrompt: ' @@user\nIndented marker.' } },
        { id: 'main-empty', preset: { mainPrompt: '' } },
        { id: 'main-preprocess-empty-additional', root: { additionalPrompt: '' }, preset: { promptPreprocess: true } },
        { id: 'main-preprocess-empty-additional-role-tail', root: { additionalPrompt: '' },
            preset: { mainPrompt: 'Before @@user', promptPreprocess: true } },
        { id: 'system-prompt-original', character: { systemPrompt: 'Before. {{original}} After.' } },
        { id: 'system-prompt-override', character: { systemPrompt: 'Override main.' } },
        { id: 'global-note-replace', character: { replaceGlobalNote: 'Card note: {{original}}' } },
        { id: 'global-note-empty', preset: { globalNote: '' } },
        { id: 'author-note-empty', conversation: { note: '' } },
        { id: 'description-fields', character: { personality: 'Calm.', scenario: 'A quiet library.' }, preset: { promptPreprocess: true } },
        { id: 'description-empty', character: { desc: '' } },
        { id: 'persona-bound', conversation: { bindedPersona: 'persona-b' } },
        { id: 'persona-second-selected', root: { selectedPersona: 1 } },
        { id: 'persona-empty', root: { personas: [{ id: 'persona-a', name: 'Synthetic user', personaPrompt: '', note: '', icon: '' }] } },
        { id: 'order-duplicates', preset: { formatingOrder: ['main', 'main', 'chats', 'lastChat', 'lastChat', 'authorNote'] } },
        { id: 'order-minimal', preset: { formatingOrder: ['lastChat'] } },
        { id: 'order-empty', preset: { formatingOrder: [] } },
        { id: 'disabled-allbefore', messages: [
            msg('user', 'Before reset.', 'reset-a'), msg('char', 'Reset point.', 'reset-b', { disabled: 'allBefore' }),
            msg('user', 'Disabled message.', 'reset-c', { disabled: true }), msg('char', 'After reset answer.', 'reset-d'),
            msg('user', 'Truthy disabled value.', 'reset-e', { disabled: 'yes' }), msg('user', 'After reset question.', 'reset-f')] },
        { id: 'disabled-only', messages: [
            msg('user', 'Disabled one.', 'off-a', { disabled: true }), msg('char', 'Disabled two.', 'off-b', { disabled: true })] },
        { id: 'alternate-greeting', conversation: { fmIndex: 0 } },
        { id: 'trim-start-new-chat', preset: { promptSettings: ps({ trimStartNewChat: true }) } },
        { id: 'thoughts-default', messages: [
            msg('user', '<Thoughts>User thought.</Thoughts> Question with thought.', 'th-a'),
            msg('char', '<Thoughts>T1</Thoughts>Answer A<Thoughts>T2</Thoughts> tail.', 'th-b'), msg('user', 'Plain question.', 'th-c')] },
        { id: 'thoughts-depth', preset: { promptSettings: ps({ maxThoughtTagDepth: 2 }) }, messages: depthMessages },
        { id: 'thoughts-depth-partial', preset: { promptSettings: ps({ maxThoughtTagDepth: 6 }) }, messages: [
            msg('char', '<Thoughts>first</Thoughts>One.', 'tp-a'), msg('user', '<Thoughts>second</Thoughts>Two.', 'tp-b'),
            msg('char', '<Thoughts>third</Thoughts>Three.', 'tp-c'), msg('user', '<Thoughts>fourth</Thoughts>Four.', 'tp-d')] },
        { id: 'thoughts-multiline-unclosed', messages: [
            msg('char', '<Thoughts>open\nline</Thoughts>\nAfter thought.', 'tm-a'), msg('user', '<Thoughts>never closed', 'tm-b'),
            msg('char', '<Thoughts></Thoughts>Empty thought.', 'tm-c')] },
        { id: 'examples-basic', character: { exampleMessage: '<START>\n{{char}}: Hello there.\n{{user}}: Hi: with colon\ncontinued line\n<bot>: Bot prefix.\n<USER>: Upper user.\nSynthetic character: Named line' } },
        { id: 'examples-leading-text', character: { exampleMessage: 'Orphan line\n{{CHAR}}:   Spaced.\n  <start>  \n<START>\n{{user}}:' } },
        { id: 'examples-lowercase-name', character: { name: 'synth', exampleMessage: 'synth: Lower named.\n{{user}}: Reply.' } },
        { id: 'depth-prompt', character: { depth_prompt: { depth: 2, prompt: ' Depth note. ' } } },
        { id: 'depth-prompt-beyond', character: { depth_prompt: { depth: 50, prompt: 'Far depth note.' } } },
        { id: 'depth-prompt-zero', character: { depth_prompt: { depth: 0, prompt: 'End depth note.' } } },
        { id: 'post-history-instructions', character: { postHistoryInstructions: 'Post history instruction.' } },
        { id: 'gpt-merge', tokenizer: 'none', capture: 'replacer', preset: { aiModel: 'gpt4o' } },
        { id: 'gpt-examples', tokenizer: 'none', capture: 'replacer', preset: { aiModel: 'gpt4o' }, character: { exampleMessage: examples } },
        { id: 'reverse-proxy-merge', tokenizer: 'none', capture: 'replacer', preset: proxy, character: { exampleMessage: examples } },
        { id: 'custom-examples-names', character: { exampleMessage: examples } },
        { id: 'escaped-private-use', messages: [msg('user', escaped, 'esc-a')] },
        { id: 'budget-exact', preset: { maxContext: 187 } },
        { id: 'budget-trim-marker', preset: { maxContext: 186 } },
        { id: 'budget-trim-greeting', preset: { maxContext: 175 } },
        { id: 'budget-output-clamp', preset: { maxContext: 240, formatingOrder: ['main', 'chats', 'chats', 'chats', 'chats', 'chats', 'chats', 'lastChat'] } },
        { id: 'recheck-trim', preset: { mainPrompt: '', globalNote: '', maxContext: 110, maxResponse: 0,
            formatingOrder: ['main', 'chats', 'chats', 'chats', 'chats', 'lastChat'] },
        character: { desc: '', firstMessage: '' }, conversation: { note: '' }, messages: longMessages },
        { id: 'recheck-failure', preset: recheckFailure },
        { id: 'template-basic', preset: { promptTemplate: [
            mainPlain, { type: 'persona', innerFormat: '<persona>{{slot}}</persona>' },
            { type: 'description', innerFormat: '<desc>{{slot}}</desc>' }, chatAll, globalPlain,
            { type: 'authornote', innerFormat: '<note>{{slot}}</note>', defaultText: 'Default note.' }] } },
        { id: 'template-author-default', conversation: { note: '' }, preset: { promptTemplate: [
            mainPlain, chatAll, { type: 'authornote', innerFormat: '<note>{{slot}}</note>', defaultText: 'Default note.' }] } },
        { id: 'template-roles-jailbreak', root: { jailbreakToggle: true, chainOfThought: true }, preset: { promptTemplate: rolesTemplate } },
        { id: 'template-toggles-off', preset: { promptTemplate: rolesTemplate } },
        { id: 'template-chatml', preset: { promptTemplate: [{ type: 'chatML', text: chatml }, chatAll] } },
        { id: 'template-chat-ranges', preset: { promptTemplate: [
            mainPlain, { type: 'chat', rangeStart: 0, rangeEnd: -2 }, globalPlain, { type: 'chat', rangeStart: -2, rangeEnd: 'end' }] } },
        { id: 'template-chat-range-edges', preset: { promptTemplate: [
            { type: 'chat', rangeStart: -1000, rangeEnd: 0 }, { type: 'chat', rangeStart: 3, rangeEnd: 1 },
            { type: 'chat', rangeStart: -50, rangeEnd: -3 }, { type: 'chat', rangeStart: 1, rangeEnd: 2 }] } },
        { id: 'template-chat-as-system', preset: { promptSettings: ps({ sendChatAsSystem: true }), promptTemplate: [
            mainPlain, chatAll, { type: 'chat', rangeStart: -1, rangeEnd: 'end', chatAsOriginalOnSystem: true }] },
        character: { exampleMessage: examples } },
        { id: 'template-cache-points', root: { automaticCachePoint: true }, preset: { promptTemplate: [
            mainPlain, chatAll, { type: 'cache', depth: 2, role: 'user' }, { type: 'cache', depth: 1, role: 'all' }] } },
        { id: 'template-auto-cache-point', root: { automaticCachePoint: true }, preset: { promptTemplate: [mainPlain, chatAll] },
            messages: [msg('user', 'Q1.', 'ac-a'), msg('char', 'A1.', 'ac-b'), msg('user', 'Q2.', 'ac-c'), msg('char', 'A2.', 'ac-d'),
                msg('user', 'Q3.', 'ac-e'), msg('user', 'Q4.', 'ac-f')] },
        { id: 'template-position-placeholder', preset: { promptTemplate: [
            { type: 'plain', type2: 'main', text: 'Main {{position::slot1}} end.', role: 'system' }, chatAll] } },
        { id: 'template-post-end-format', root: { chainOfThought: true }, preset: {
            promptSettings: ps({ postEndInnerFormat: 'Final format line.' }), promptTemplate: [mainPlain, chatAll] } },
        { id: 'template-custom-cot', root: { chainOfThought: true }, preset: {
            promptSettings: ps({ customChainOfThought: true }), promptTemplate: [mainPlain, chatAll] } },
        { id: 'template-send-name', preset: { promptSettings: ps({ sendName: true }), namedMessageRole: 'system',
            promptTemplate: [mainPlain, chatAll] } },
        { id: 'template-send-name-custom', preset: { promptSettings: ps({ sendName: true }), messageNameTemplate: '[{{slot}}]',
            namedMessageRole: 'unknown', promptTemplate: [mainPlain, chatAll] } },
        { id: 'utility-bot', character: { utilityBot: true } },
        { id: 'utility-bot-template-override', character: { utilityBot: true }, preset: {
            promptSettings: ps({ utilOverride: true }), promptTemplate: [mainPlain, chatAll] } },
        { id: 'utility-bot-template-replaced', character: { utilityBot: true }, preset: { promptTemplate: [mainPlain, chatAll] } },
        { id: 'inlay-view-imggen', character: { inlayViewScreen: true, viewScreen: 'imggen', newGenData: {
            prompt: '', negative: '', instructions: 'Image instructions.', emotionInstructions: '' } } },
        { id: 'inlay-view-emotion', character: { inlayViewScreen: true, viewScreen: 'emotion',
            emotionImages: [['happy', 'happy.png'], ['sad', 'sad.png']],
            newGenData: { prompt: '', negative: '', instructions: '', emotionInstructions: 'Pick one of {{slot}}.' } } },
        { id: 'plugin-tokenizer-caching', root: { useTokenizerCaching: true }, preset: {
            currentPluginProvider: 'synthetic-cache-provider', formatingOrder: ['main', 'chats', 'lastChat', 'main', 'globalNote', 'globalNote'] } },

        // What TS stores when a send fails after a stage changed the conversation.
        { id: 'failure-parser-setvar-context-limit', preset: { maxContext: 20 },
            messages: [msg('user', 'Question {{setvar::parsed::yes}}one.', 'pv-a')], decline: 'historyRewrite' },
        { id: 'trigger-setvar', character: { triggerscript: startTrigger(setvar('started', 'yes')) } },
        { id: 'failure-trigger-setvar-context-limit', preset: { maxContext: 20 },
            character: { triggerscript: startTrigger(setvar('started', 'yes')) } },
        { id: 'failure-trigger-setvar-recheck', preset: recheckFailure,
            character: { triggerscript: startTrigger(setvar('started', 'yes')) } },
        { id: 'failure-trigger-stop', character: { triggerscript: startTrigger(setvar('started', 'yes'), { type: 'stop' }) } },
        // The history the prompt reads is the start trigger's chat, selected again.
        { id: 'trigger-modifychat', character: { triggerscript: startTrigger({ type: 'modifychat', index: '2', value: 'Edited question.' }) } },
        { id: 'trigger-cutchat', character: { triggerscript: startTrigger({ type: 'cutchat', start: '1', end: '3' }) } },
        { id: 'trigger-cutchat-removes-allbefore', messages: [
            msg('user', 'Before reset.', 'cut-a'), msg('char', 'Reset point.', 'cut-b', { disabled: 'allBefore' }),
            msg('user', 'After reset.', 'cut-c')],
            character: { triggerscript: startTrigger({ type: 'cutchat', start: '0', end: '1' }) } },
        { id: 'failure-trigger-modifychat-stop',
            character: { triggerscript: startTrigger({ type: 'modifychat', index: '0', value: 'Edited first.' }, { type: 'stop' }) } },

        // GetSubstitution patterns in the replacement of a `{{slot}}` or `{{original}}` placeholder.
        { id: 'dollar-slot-replacements',
            root: { personas: [{ id: 'persona-a', name: 'Synthetic user', personaPrompt: "Pay $$5, keep $& and $` and $' here.", note: '', icon: '' }] },
            preset: { promptSettings: ps({ sendName: true }), messageNameTemplate: '[{{slot}}]', promptTemplate: [
                mainPlain, { type: 'persona', innerFormat: '<persona>{{slot}}</persona>' }, chatAll,
                { type: 'plain', type2: 'globalNote', text: "Global $$ and $` and $'.", role: 'system' }] },
            character: { replaceGlobalNote: 'Card {{original}} note.' },
            messages: [msg('user', 'Cost $$1, $& and $` end.', 'ds-a')] },
        { id: 'dollar-original-replacements', preset: { mainPrompt: "Main $$ and $` and $'.", globalNote: "Global $$ $` $'." },
            character: { systemPrompt: 'Before {{original}} after.', replaceGlobalNote: 'Card {{original}} note.' } },
        { id: 'claude-merge', tokenizer: 'none', capture: 'replacer', preset: { aiModel: 'claude-synthetic' }, character: { exampleMessage: examples } },
        { id: 'novelai-no-new-chat', tokenizer: 'none', capture: 'replacer', preset: { aiModel: 'novelai-synthetic' },
            character: { exampleMessage: examples } },
        { id: 'nickname', preset: { promptSettings: ps({ sendName: true }), promptTemplate: [
            mainPlain, { type: 'description', innerFormat: '<desc>{{slot}}</desc>' }, chatAll] },
        character: { nickname: 'nick', desc: 'About {{char}}.',
            exampleMessage: '<START>\n{{char}}: Hello.\nnick: Not a name line.\nSynthetic character: Named line' } },
        { id: 'template-chatml-separators', preset: { promptTemplate: [{ type: 'chatML', text: chatmlSeparators }, chatAll] } },
        { id: 'biases', preset: { bias: [['Line\\nbreak', 5], ['{{char}} name', -3]] }, character: { bias: [['Back\\\\slash\\r', 1.5]] } },
        // The depth check reads a stored value that is not a number as `Number(depth)`.
        { id: 'thoughts-depth-text', preset: { promptSettings: ps({ maxThoughtTagDepth: '2' }) }, messages: depthMessages },
        { id: 'thoughts-depth-object', preset: { promptSettings: ps({ maxThoughtTagDepth: {} }) }, messages: depthMessages },
    ]

    const lore = (content, extra = {}) => ({ key: '', secondkey: '', insertorder: 100, comment: '', content,
        mode: 'constant', alwaysActive: true, selective: false, ...extra })
    const placed = (position, text, role = 'system', extra = {}) =>
        lore(`@@position ${position}\n@@role ${role}\n${text}`, extra)
    const injected = (site, text, operation = '', parameter = '') =>
        lore(`@@inject_at ${site}\n${operation ? `@@inject_${operation} ${parameter}\n` : ''}${text}`)
    const descriptionLore = [placed('before_desc', 'Before user.', 'user'), placed('before_desc', 'Before assistant.', 'assistant'),
        placed('after_desc', 'After system.'), placed('personality', 'Lore personality.', 'user'), placed('scenario', 'Lore scenario.', 'assistant')]
    const injectionTemplate = [
        { type: 'persona', innerFormat: 'Persona {{slot}}', role2: 'user' },
        { type: 'description', innerFormat: 'Description {{slot}}', role2: 'bot' },
        { type: 'authornote', innerFormat: 'Note {{slot}}' },
        { type: 'plain', type2: 'main', text: 'Main target target.', role: 'system' },
        { type: 'plain', type2: 'normal', text: 'Plain target target.', role: 'user' },
        { type: 'plain', type2: 'globalNote', text: 'Global target target.', role: 'system' },
        { type: 'jailbreak', type2: 'normal', text: 'Jail target target.', role: 'system' },
        { type: 'cot', type2: 'normal', text: 'Cot target target.', role: 'system' }, chatAll,
    ]
    const depthLore = [lore('@@depth 1\nOrdinary one.'), lore('@@depth 99\nOrdinary beyond.'),
        lore('@@depth -1\nExcluded ordinary negative.'), lore('@@reverse_depth 0\nReverse zero.'),
        lore('@@reverse_depth 2\nReverse two.'), lore('@@reverse_depth 99\nReverse beyond.'),
        lore('@@reverse_depth -2\nReverse negative.')]
    const addLore = (id, entries, values = {}) => cases.push({ id, lorePlacement: true, tokenizer: 'cl100k_base', ...values,
        character: { loreSettings: { tokenBudget: 10000, scanDepth: 5, recursiveScanning: false },
            ...values.character, globalLore: entries } })
    addLore('lore-normal-roles-order', [lore('System low.', { insertorder: 10 }),
        lore('@@role user\nUser high.', { insertorder: 30 }), lore('@@role assistant\nAssistant middle.', { insertorder: 20 }),
        lore('Same order first.', { insertorder: 40 }), lore('Same order second.', { insertorder: 40 })])
    addLore('lore-description-groups', descriptionLore, { character: { personality: 'Base personality.', scenario: 'Base scenario.' } })
    addLore('lore-description-repeated-role2', descriptionLore, { preset: { promptTemplate: [
        { type: 'description', innerFormat: '[{{slot}}]', role2: 'bot' },
        { type: 'description', innerFormat: '<{{slot}}>', role2: 'user' }, chatAll] } })
    addLore('lore-named-nested-missing', [placed('pt_outer', 'Outer {{position::inner}}.'), placed('pt_inner', 'Inner {{char}}.'),
        lore('Normal {{position::outer}} / {{position::missing}}.')], { preset: { promptTemplate: [
            { type: 'plain', type2: 'main', role: 'system', text: 'Main {{position::outer}} / {{position::missing}}.' },
            { type: 'lorebook' }, chatAll] } })
    addLore('lore-named-cycle-five-rounds', [placed('pt_cycle', 'C{{position::cycle}}'),
        ...Array.from({ length: 6 }, (_, index) => placed(`pt_n${index}`, `N${index}{{position::n${index + 1}}}`)),
        lore('Cycle {{position::cycle}}; chain {{position::n0}}.')])
    addLore('lore-injection-template-sites', [injected('persona', 'Persona prepend.', 'prepend'),
        injected('description', 'Description append.'), injected('authornote', 'Note append.'),
        injected('main', 'Main append.'), injected('normal', 'Plain prepend.', 'prepend'),
        injected('globalNote', 'Global append.'), injected('jailbreak', 'Jail append.'), injected('cot', 'Cot append.')],
        { root: { jailbreakToggle: true, chainOfThought: true }, character: { replaceGlobalNote: 'Replacement {{original}}.' },
            preset: { promptTemplate: injectionTemplate } })
    addLore('lore-injection-first-dollar-empty', [injected('main', "<$$|$&|$`|$'>", 'replace', 'target'),
        injected('normal', 'Empty prefix.', 'replace', '')], { preset: { promptTemplate: injectionTemplate } })
    addLore('lore-injection-excluded-sites', [injected('chatML', 'Excluded ChatML.'), injected('memory', 'Excluded memory.'),
        injected('persona', 'No inner format.'), placed('pt_unused', 'Unused position.')], { preset: { promptTemplate: [
            { type: 'chatML', text: chatml }, { type: 'memory', innerFormat: 'Memory {{slot}}' },
            { type: 'persona' }, chatAll] } })
    addLore('lore-injection-no-template', [injected('main', 'Excluded main.'), injected('description', 'Excluded description.'),
        injected('persona', 'Excluded persona.'), injected('authornote', 'Excluded note.')])
    addLore('lore-depth-zero-role-order', [lore('@@depth 0\n@@role assistant\nAssistant first.'),
        lore('@@depth 0\n@@role user\nUser second.'), lore('@@depth 0\nSystem third.'),
        lore('@@depth 0\n@@role assistant\nAssistant fourth.')])
    addLore('lore-depth-sequential-bounds', depthLore)
    addLore('lore-depth-template-sequential', depthLore, { preset: { promptTemplate: [mainPlain, chatAll, { type: 'postEverything' }] } })
    addLore('lore-count-trim', [lore('@@depth 1\nAdditional depth context consumes tokens before trimming history.')], {
        tokenizer: 'cl100k_base', preset: { maxContext: 120, maxResponse: 0 }, messages: longMessages })
    addLore('lore-depth-repeated-cbs', [lore('@@depth 1\nDepth {{random::A::BBBBBBBBBBBB}}.'),
        lore('@@reverse_depth 1\nReverse {{random::C::DDDDDDDDDDDD}}.')], {
        tokenizer: 'cl100k_base', random: [0.05, 0.95, 0.95, 0.05, 0.05, 0.95, 0.95, 0.05, 0.05, 0.95, 0.95, 0.05] })

    addLore('lore-template-repeated-cbs', [injected('persona', '{{random::A::BBBBBBBBBBBB}}')], {
        preset: { promptTemplate: [{ type: 'persona', innerFormat: '[{{slot}}]' },
            { type: 'persona', innerFormat: '<{{slot}}>' }, chatAll] },
        random: [0.05, 0.95, 0.05, 0.05, 0.95, 0.95, 0.05, 0.95] })

    write('send', { schema: 1, base, cases })
    return cases.length
}

// Every root field a preset or persona mirrors (`presetMirrorMap`, then `personaMirrorMap`).
const mirrorKeys = [
    'apiType', 'localNetworkMode', 'localNetworkTimeoutSec', 'mainPrompt', 'jailbreak', 'globalNote', 'temperature', 'maxContext',
    'maxResponse', 'frequencyPenalty', 'PresensePenalty', 'formatingOrder', 'aiModel', 'subModel', 'currentPluginProvider', 'textgenWebUIStreamURL',
    'textgenWebUIBlockingURL', 'forceReplaceUrl', 'promptPreprocess', 'bias', 'koboldURL', 'proxyKey', 'ooba', 'ainconfig',
    'proxyRequestModel', 'openrouterRequestModel', 'promptTemplate', 'NAIadventure', 'NAIappendName', 'localStopStrings', 'autoSuggestPrompt', 'autoSuggestPrefix',
    'autoSuggestClean', 'customProxyRequestModel', 'reverseProxyOobaArgs', 'top_p', 'promptSettings', 'repetition_penalty', 'min_p', 'top_a',
    'openrouterProvider', 'useInstructPrompt', 'customPromptTemplateToggle', 'templateDefaultVariables', 'moduleIntergration', 'top_k', 'instructChatTemplate', 'JinjaTemplate',
    'jsonSchemaEnabled', 'jsonSchema', 'strictJsonSchema', 'extractJson', 'namedMessageRole', 'messageNameTemplate', 'seperateParametersEnabled', 'seperateParameters',
    'customAPIFormat', 'systemContentReplacement', 'systemRoleReplacement', 'customFlags', 'enableCustomFlags', 'thinkingTokens', 'thinkingType', 'deepseekThinkingType',
    'adaptiveThinkingEffort', 'deepseekReasoningEffort', 'outputImageModal', 'seperateModelsForAxModels', 'seperateModels', 'modelTools', 'fallbackModels', 'fallbackWhenBlankResponse',
    'verbosity', 'dynamicOutput', 'NAIsettings', 'presetRegex', 'reasoningEffort', 'username', 'userIcon', 'personaPrompt',
    'userNote',
]

function writeSettings() {
    const base = {
        root: {
            formatversion: 3, botPresetsId: 0, selectedPersona: 0,
            personas: [
                { id: 'persona-a', name: 'Synthetic user', personaPrompt: 'Synthetic persona.', note: 'Synthetic note.', icon: 'persona-icon' },
                { id: 'persona-b', name: 'Second user', personaPrompt: 'Second persona.', note: '', icon: '' },
            ],
            mainPrompt: 'Inactive root main.', temperature: 99, maxContext: 1, promptSettings: { sendName: true },
            seperateModels: { memory: 'root-memory', emotion: '', translate: '', otherAx: '' },
            seperateModelsForAxModels: true,
            fallbackModels: { model: ['root-fallback'], memory: [], emotion: [], translate: [], otherAx: [] },
            fallbackWhenBlankResponse: true,
            seperateParameters: { memory: { temperature: 3 }, emotion: {}, translate: {}, otherAx: {}, overrides: {} },
            username: 'Inactive root user', requestRetrys: 0, plugins: [], modules: [], enabledModules: [], loadouts: [],
        },
        preset: {
            id: 'preset-a', name: 'Synthetic preset', aiModel: 'custom', currentPluginProvider: 'synthetic-provider',
            mainPrompt: 'Synthetic main.', jailbreak: 'Synthetic jailbreak.', globalNote: 'Synthetic global note.',
            maxContext: 4000, maxResponse: 64, temperature: 70, promptTemplate: null, promptPreprocess: false,
            formatingOrder: ['main', 'description', 'personaPrompt', 'chats', 'lastChat', 'jailbreak', 'lorebook', 'globalNote', 'authorNote'],
        },
    }
    const models = (memory) => ({ memory, emotion: '', translate: '', otherAx: '' })
    const parameters = (memory) => ({ memory, emotion: {}, translate: {}, otherAx: {}, overrides: {} })
    const cases = [
        { id: 'preset-values' },
        { id: 'preset-minimal', preset: { aiModel: null, currentPluginProvider: null, mainPrompt: null, jailbreak: null, globalNote: null,
            maxContext: null, maxResponse: null, temperature: null, promptTemplate: null, promptPreprocess: null, formatingOrder: null } },
        { id: 'preset-explicit-nulls', preset: { promptSettings: null, NAISettings: null, regex: null, reasonEffort: null, top_p: null,
            bias: null, customFlags: null, seperateModels: null, fallbackModels: null, modelTools: null, verbosity: null, thinkingTokens: null } },
        { id: 'preset-raw-fields', preset: { NAIadventure: true, NAIappendName: false, localStopStrings: ['stop'], repetition_penalty: 1.1,
            min_p: 0.05, top_a: 0.2, openrouterProvider: { order: ['a'] }, dynamicOutput: { enabled: true }, top_k: 40, top_p: 0.9 } },
        { id: 'preset-prompt-settings-partial', preset: { promptSettings: { sendName: true, sendChatAsSystem: true } } },
        { id: 'preset-prompt-settings-depth', preset: { promptSettings: { assistantPrefill: '', postEndInnerFormat: '', sendChatAsSystem: false,
            sendName: false, utilOverride: false, maxThoughtTagDepth: 2 } } },
        { id: 'preset-nai-partial', preset: { NAISettings: { topK: 5, cfg_scale: 2 } } },
        { id: 'preset-template-roles', preset: { promptTemplate: [
            { type: 'plain', type2: 'normal', text: 'A', role: 'assistant' },
            { type: 'jailbreak', type2: 'normal', text: 'B', role: 'char' },
            { type: 'cot', type2: 'normal', text: 'C', role: 'unknown' },
            { type: 'persona', role2: 'assistant' },
            { type: 'description', role2: 'weird' },
            { type: 'authornote' },
            { type: 'memory', role2: null },
            { type: 'cache', depth: 1, role: 'bot' },
            { type: 'cache', depth: 2, role: 'nobody' },
            { type: 'chat', rangeStart: 0, rangeEnd: 'end' },
            null,
        ] } },
        { id: 'preset-template-object', preset: { promptTemplate: { type: 'plain' } } },
        { id: 'preset-renamed-fields', preset: { regex: [{ type: 'editprocess', in: 'a', out: 'b' }], reasonEffort: 3, NAISettings: { topK: 2 } } },
        { id: 'protected-separate-models-stored', root: { doNotChangeSeperateModels: true, protectedPresetValues: {
            seperateModels: models('protected-memory'), seperateModelsForAxModels: false } },
        preset: { seperateModels: models('preset-memory'), seperateModelsForAxModels: true } },
        { id: 'protected-separate-models-root', root: { doNotChangeSeperateModels: true }, preset: { seperateModels: models('preset-memory') } },
        { id: 'protected-separate-models-off', root: { doNotChangeSeperateModels: false, protectedPresetValues: {
            seperateModels: models('protected-memory') } }, preset: { seperateModels: models('preset-memory') } },
        { id: 'protected-fallback-partial', root: { doNotChangeFallbackModels: true, protectedPresetValues: { fallbackWhenBlankResponse: false } },
            preset: { fallbackModels: { model: ['preset-fallback'], memory: [], emotion: [], translate: [], otherAx: [] }, fallbackWhenBlankResponse: true } },
        { id: 'protected-parameters', root: { disableSeperateParameterChangeOnPresetChange: true },
            preset: { seperateParameters: parameters({ temperature: 1 }) } },
        { id: 'protected-parameters-stored', root: { disableSeperateParameterChangeOnPresetChange: true,
            protectedPresetValues: { seperateParameters: parameters({ top_p: 0.5 }) } } },
        { id: 'preset-parameters-own', preset: { seperateParameters: parameters({ temperature: 1 }) } },
        { id: 'persona-second', root: { selectedPersona: 1 } },
        { id: 'persona-missing-fields', root: { personas: [{ id: 'persona-a', name: 'Only name' }] } },
        { id: 'persona-out-of-range', root: { selectedPersona: 7 } },
        // An edit saved in session keeps a selection that the whole-database write stores as an empty ID.
        { id: 'persona-unknown-id', root: { selectedPersona: 1 }, edit: { set: { selectedPersona: 'persona-missing' } } },
        { id: 'persona-absent', root: { selectedPersona: 1 }, edit: { removed: ['selectedPersona'] } },
        { id: 'persona-negative', root: { selectedPersona: 1 }, edit: { set: { selectedPersona: -1 } } },
        { id: 'persona-fractional', root: { selectedPersona: 1 }, edit: { set: { selectedPersona: 0.5 } } },
        { id: 'persona-non-number', root: { selectedPersona: 1 }, edit: { set: { selectedPersona: true } } },
        // The load gives a root stored without a persona list one persona made from the root mirrors and an empty prompt,
        // and selects it unless the selection is negative.
        { id: 'persona-list-absent', stored: { removed: ['personas'] } },
        { id: 'persona-list-absent-unknown-id', stored: { removed: ['personas'] }, edit: { set: { selectedPersona: 'persona-missing' } } },
        { id: 'persona-list-absent-negative', stored: { removed: ['personas'] }, edit: { set: { selectedPersona: -1 } } },
        { id: 'preset-second-selected', root: { botPresetsId: 1 } },
        { id: 'order-without-persona', preset: { formatingOrder: ['main', 'chats', 'lastChat'] } },
        { id: 'order-without-main', preset: { formatingOrder: ['chats', 'lastChat'] } },
        { id: 'order-empty', preset: { formatingOrder: [] } },
        { id: 'preset-negative', preset: { formatingOrder: ['main', 'chats', 'lastChat'] }, edit: { set: { botPresetsId: -1 } } },
        // The startup load gives a stored selection that matches no preset the first preset.
        { id: 'preset-out-of-range', preset: { formatingOrder: ['main', 'chats', 'lastChat'] }, edit: { set: { botPresetsId: 5 } } },
        { id: 'preset-fractional', preset: { formatingOrder: ['main', 'chats', 'lastChat'] }, edit: { set: { botPresetsId: 0.5 } } },
        { id: 'preset-non-number', preset: { formatingOrder: ['main', 'chats', 'lastChat'] }, edit: { set: { botPresetsId: true } } },
        { id: 'preset-null', preset: { formatingOrder: ['main', 'chats', 'lastChat'] }, edit: { set: { botPresetsId: null } } },
        // With no preset or persona selected, each mirror is the root field as the load normalizes it.
        { id: 'preset-negative-root-defaults', edit: { set: { botPresetsId: -1, selectedPersona: -1 }, removed: mirrorKeys } },
        { id: 'preset-negative-root-raw', edit: { set: { botPresetsId: -1, localNetworkMode: 'yes', localNetworkTimeoutSec: '600',
            top_p: '1', NAIadventure: null, openrouterProvider: 'synthetic-provider',
            fallbackModels: { emotion: [''], memory: ['memory-model', ''], model: ['', 'fallback-model'], otherAx: [], translate: [] },
            NAIsettings: { topK: 1 }, promptSettings: { assistantPrefill: 'Synthetic prefill.' }, seperateParameters: { memory: {} },
            promptTemplate: [{ type: 'plain', type2: 'normal', role: 'assistant', text: 'Synthetic card.' }], formatingOrder: ['chats'] } } },
    ]
    write('settings', { schema: 1, base, cases })
    return cases.length
}

console.log(process.argv.includes('--send-only') ? `send ${writeSend()}` : `send ${writeSend()}, settings ${writeSettings()}`)
