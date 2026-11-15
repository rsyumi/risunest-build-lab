// Writes tests/fixtures/rust-core/response/application.cases.json, the synthetic replies of the response oracle
// (`response.oracle.test.ts`). Each case overrides the Ollama baseline (`tests/fixtures/native-generation/ollama-text.json`):
// `root`, `preset`, `character` and `conversation` are merged into it and `messages` replaces its messages. `reply` is
// what the synthetic provider answers: `{ content }` from Ollama, or `{ multiline }` as OpenAI choices of a `gpt` model
// with `genTime` 2 over plain fetch. `hooks` registers that many `editoutput` plugin hooks, answered in call order by `replies`
// (`{ value }`, `{ suffix }` appended to the input, `{ null }`, `{ undefined }`, `{ throw }`); `listeners` registers
// `chatOutput` listeners. `decline` names what the Rust stage declines before any host request; TS still runs the case.
// Add cases here, run `node benchmarks/rust-core-oracles/response.cases.mjs`, then regenerate the expected file with
// RUST_CORE_ORACLE_WRITE=1.
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const cases = []
const add = (id, values) => cases.push({ id, ...values })
const script = (pattern, out, type = 'editoutput', flag) => ({
    comment: '', in: pattern, out, type, ...(flag === undefined ? {} : { flag, ableFlag: true }),
})
const message = (role, data, chatId) => ({ role, data, chatId })
const history = [
    message('user', ' First question. ', 'message-a'),
    message('char', '<Thoughts>Previous hidden thought.</Thoughts> First answer. ', 'message-b'),
    message('user', ' Second question. ', 'message-c'),
]
// A continued reply extends the last message, which the send leaves in place.
const continued = (data) => [...history, { role: 'char', data, chatId: 'continue-base', saying: 'character', time: 100 }]
const emotions = [['Happy', 'happy-asset'], ['Sad', 'sad-asset']]
// Prompt preparation reads the inlay instructions, which `updateInlayScreen` fills in the app.
const genData = (fields) => ({ prompt: '', negative: '', instructions: '', emotionInstructions: '', ...fields })
const emotionInlay = { inlayViewScreen: true, viewScreen: 'emotion', emotionImages: emotions, newGenData: genData({ emotionInstructions: 'Pick one of {{slot}}.' }) }

// Trimming and CBS.
add('plain-trim', { reply: { content: '  Synthetic reply.\n ' } })
add('plain-empty', { reply: { content: ' \n\t ' } })
add('cbs-char', { reply: { content: 'Hello {{char}}.' } })
add('cbs-getvar', { conversation: { scriptstate: { $mood: 'calm' } }, reply: { content: 'Mood: {{getvar::mood}}.' } })
add('cbs-setvar-pass', { reply: { content: 'Hello{{setvar::mood::happy}} there.' } })
add('cbs-emotion-literal', { reply: { content: 'Look {{emotion::Happy}}.' } })

// Regex scripts in editoutput mode.
add('script-replace', {
    character: { customscript: [
        script('cat', 'dog', 'editoutput', 'g'), script('cat', 'cow', 'editinput', 'g'),
        script('cat', 'fish', 'editprocess', 'g'), script('cat', 'bird', 'editdisplay', 'g'),
    ] },
    reply: { content: ' A cat and a cat. ' },
})
add('script-order', { character: { customscript: [script('a', 'b', 'editoutput', 'g'), script('b', 'c', 'editoutput', 'g')] }, reply: { content: 'aab' } })
add('script-preset-first', {
    preset: { regex: [script('Hello', 'Hi', 'editoutput', 'g')] },
    character: { customscript: [script('Hi', 'Hey', 'editoutput', 'g')] },
    reply: { content: 'Hello there.' },
})
add('script-cbs-replacement', { character: { customscript: [script('NAME', '{{char}}', 'editoutput', 'g')] }, reply: { content: 'I am NAME.' } })
add('script-emo', {
    character: { emotionImages: emotions, customscript: [script('smile', '@@emo Happy')] },
    reply: { content: 'I smile.' },
})
add('script-emo-unknown', {
    character: { emotionImages: emotions, customscript: [script('smile', '@@emo Missing')] },
    reply: { content: 'I smile.' },
})
add('script-inject-new', { character: { customscript: [script('secret', '@@inject')] }, reply: { content: 'A secret word.' } })
add('script-repeat-back-new', { character: { customscript: [script('First', '@@repeat_back end')] }, reply: { content: 'Reply.' } })
add('script-move-bottom', { character: { customscript: [script('\\[note\\]', '@@move_bottom', 'editoutput', 'g')] }, reply: { content: '[note] Body text.' } })

// Incomplete-response removal and the inlay.
add('incomplete-trim', { root: { removeIncompleteResponse: true }, reply: { content: 'Done. And then' } })
add('incomplete-astral', { root: { removeIncompleteResponse: true }, reply: { content: 'Hi! Wow 🦊' } })
add('incomplete-none', { root: { removeIncompleteResponse: true }, reply: { content: 'no stop here' } })
add('incomplete-cjk', { root: { removeIncompleteResponse: true }, reply: { content: '좋아요。 그리고 다음' } })
add('inlay-emotion', {
    character: emotionInlay,
    reply: { content: 'Hi <Emotion="Happy"> and <emotion="Sad">.' },
})
add('inlay-emotion-lazy', {
    character: emotionInlay,
    reply: { content: 'A <Emotion="a">b"> B <Emotion="">x"> C <Emotion="line\nbreak"> D <EMOTION="Up">' },
})
// The inlay replaces tags only on the emotion screen; without the inlay the emotion screen classifies after the stage.
add('inlay-other-screen', { character: { inlayViewScreen: true, viewScreen: 'none', emotionImages: emotions }, reply: { content: 'Keep <Emotion="Happy">.' } })

// Plugin editoutput hooks, recorded as host tapes.
add('hook-replace', { hooks: 1, replies: [{ value: 'Hooked {{char}}.' }], reply: { content: ' raw ' } })
add('hook-chain', { hooks: 2, replies: [{ null: true }, { suffix: ' [two]' }], reply: { content: 'Base.' } })
add('hook-undefined', { hooks: 1, replies: [{ undefined: true }], reply: { content: 'Kept.' } })
add('hook-empty-string', { hooks: 1, replies: [{ value: '' }], reply: { content: 'Gone.' } })
add('hook-throw', { hooks: 1, replies: [{ throw: 'Synthetic hook failure.' }], reply: { content: '  Raw reply.  ' } })
add('hook-then-script', {
    hooks: 1, replies: [{ suffix: ' cat' }],
    character: { customscript: [script('cat', 'dog', 'editoutput', 'g')] },
    reply: { content: 'A cat' },
})

// Continue: the reply extends the last message, which runs the scripts twice.
add('continue-plain', { messages: continued('Prefix: '), continue: true, reply: { content: ' more words.' } })
add('continue-trim-base', { messages: continued('  Spaced base'), continue: true, reply: { content: ' tail  ' } })
add('continue-hooks', { messages: continued('Base'), continue: true, hooks: 1, replies: [{ suffix: ' [a]' }, { suffix: ' [b]' }], reply: { content: ' next' } })
add('continue-hook-throw', {
    messages: continued('Base'), continue: true, hooks: 1, replies: [{ suffix: ' [a]' }, { throw: 'Second run failure.' }],
    reply: { content: ' next ' },
})
add('continue-inject', { messages: continued('Before: '), continue: true, character: { customscript: [script('secret', '@@inject')] }, reply: { content: 'secret word.' } })
add('continue-repeat-back', {
    messages: continued('Prefix:'), continue: true, character: { customscript: [script('First', '@@repeat_back end')] },
    reply: { content: ' Reply.' },
})
add('continue-emo-twice', {
    messages: continued('I'), continue: true,
    character: { emotionImages: emotions, customscript: [script('smile', '@@emo Happy')] },
    reply: { content: ' smile.' },
})
add('continue-empty', { messages: [], continue: true, reply: { content: 'Nothing to continue.' } })
add('continue-incomplete', { messages: continued('Start. '), continue: true, root: { removeIncompleteResponse: true }, reply: { content: 'middle' } })

// Multiple outputs become response variants of one message.
add('multiline-two', { reply: { multiline: [' First option. ', 'Second option.'] } })
add('multiline-scripts', {
    character: { customscript: [script('cat', 'dog', 'editoutput', 'g')] }, hooks: 1,
    replies: [{ suffix: ' [1]' }, { suffix: ' [2]' }, { suffix: ' [3]' }],
    reply: { multiline: ['cat one', 'cat two', 'cat three'] },
})
add('multiline-setvar', { reply: { multiline: ['Pick{{setvar::pick::one}}', 'Other{{setvar::pick::two}}'] } })
add('multiline-incomplete', { root: { removeIncompleteResponse: true }, reply: { multiline: ['One. two', 'Three'] } })
add('multiline-emo', {
    character: { emotionImages: emotions, customscript: [script('smile', '@@emo Happy'), script('cry', '@@emo Sad')] },
    reply: { multiline: ['I smile.', 'I cry.'] },
})
add('multiline-hook-throw', { hooks: 1, replies: [{ suffix: ' [1]' }, { throw: 'Second output failure.' }], reply: { multiline: ['First. ', ' Second. '] } })
// A reroll in progress records every output commit in its recovery outputs.
add('reroll-recovery-outputs', {
    conversation: { rerollRecovery: { attemptId: 'attempt-a', phase: 'generating', startIndex: 3, anchorId: 'message-c', original: [], responseCount: 1, outputs: {} } },
    root: { genTime: 2 }, reply: { multiline: ['Rerolled one.', 'Rerolled two.'] },
})
add('reroll-recovery-continue', {
    messages: continued('Base'), continue: true,
    conversation: { rerollRecovery: { attemptId: 'attempt-b', phase: 'generating', startIndex: 3, original: [], responseCount: 1, outputs: { 'continue-base': { role: 'char', data: 'Old' } } } },
    reply: { content: ' more.' },
})
add('reroll-recovery-prepared', {
    conversation: { rerollRecovery: { attemptId: 'attempt-c', phase: 'prepared', startIndex: 3, original: [], responseCount: 1, outputs: {} } },
    reply: { content: 'Prepared only.' },
})

// The current-chat pass parses every stored message with runVar.
add('cbs-pass-history', {
    messages: [message('user', 'Hi{{setvar::seen::1}}', 'message-a'), message('char', 'Seen {{getvar::seen}}.', 'message-b')],
    reply: { content: 'Reply {{getvar::seen}}.' },
})

// A trigger that does not run in output mode leaves the reply to the stage.
add('trigger-start-only', {
    character: { triggerscript: [{ comment: '', type: 'start', conditions: [], effect: [{ type: 'setvar', operator: '=', var: 'started', value: '1' }] }] },
    reply: { content: 'After start.' },
})

// Declines. Each case also registers an editoutput hook, so the decline must come before its request.
add('decline-output-trigger', {
    character: { triggerscript: [{ comment: '', type: 'output', conditions: [], effect: [{ type: 'setvar', operator: '=', var: 'seen', value: 'yes' }] }] },
    hooks: 1, replies: [{ suffix: ' [h]' }], reply: { content: 'Triggered.' }, decline: 'outputTrigger',
})
add('decline-chat-output', { listeners: 1, hooks: 1, replies: [{ suffix: ' [h]' }], reply: { content: 'Listened.' }, decline: 'chatOutput' })
add('decline-tts', { root: { ttsAutoSpeech: true }, hooks: 1, replies: [{ suffix: ' [h]' }], reply: { content: 'Spoken.' }, decline: 'ttsAutoSpeech' })
add('decline-inlay-imggen', {
    character: { inlayViewScreen: true, viewScreen: 'imggen', newGenData: genData({ prompt: 'best quality, {{slot}}', instructions: 'Describe the scene.' }) }, hooks: 1, replies: [{ suffix: ' [h]' }],
    reply: { content: 'No image tag.' }, decline: 'inlayImageGeneration',
})
add('decline-multiline-empty', { hooks: 1, replies: [{ suffix: ' [h]' }], reply: { multiline: [] }, decline: 'multilineEmpty' })

const ids = new Set()
for (const entry of cases) { if (ids.has(entry.id)) throw new Error(`duplicate case ${entry.id}`); ids.add(entry.id) }
// One case per line keeps the cases readable in review.
const text = `{"schema":1,"cases":[\n${cases.map((entry) => JSON.stringify(entry)).join(',\n')}\n]}\n`
JSON.parse(text)
writeFileSync(fileURLToPath(new URL('../../tests/fixtures/rust-core/response/application.cases.json', import.meta.url)), text)
console.log(`${cases.length} cases`)
