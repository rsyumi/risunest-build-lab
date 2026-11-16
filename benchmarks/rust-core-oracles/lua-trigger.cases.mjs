// Writes the synthetic case file of the Lua start-trigger oracle (tests/fixtures/rust-core/lua/trigger.cases.json) from the
// Lua rows of the Ollama Send fixture (tests/fixtures/native-generation/ollama-text.json), so both run the same triggers.
// Add rows there, run `node benchmarks/rust-core-oracles/lua-trigger.cases.mjs`, then regenerate the expected file with
// RUST_CORE_ORACLE_WRITE=1.
import { readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const send = JSON.parse(readFileSync(new URL('../../tests/fixtures/native-generation/ollama-text.json', import.meta.url), 'utf8'))
const output = fileURLToPath(new URL('../../tests/fixtures/rust-core/lua/trigger.cases.json', import.meta.url))

const runsLua = trigger => (trigger.effect ?? []).some(effect => effect.type === 'triggerlua')
// The selected modules' triggers as `getModuleTriggers` hands them to `runTrigger`, each with its module's low-level flag.
const moduleTriggers = row => (row.root?.modules ?? []).filter(module => (row.root?.enabledModules ?? []).includes(module.id))
    .flatMap(module => (module.trigger ?? []).map(trigger => ({ ...trigger, lowLevelAccess: module.lowLevelAccess })))

// What a start trigger reads that the send changed before it ran: the send runs the CBS of the stored messages, `setvar`
// included, and stores the rewritten data.
const beforeTrigger = {
    'lua-read-after-cbs-setvar': ({ rows, scriptstate }) => {
        rows[2].data = ' Second question. '
        scriptstate.$fromCbs = 'cbs-value'
    },
}
// The request an `editRequest` listener edits after the start trigger ran.
const requests = {
    'lua-edit-request-listener': [{ role: 'system', content: 'Synthetic main.' }, { role: 'user', content: 'Second question.' },
        { role: 'system', content: 'Synthetic author note.' }],
}

const cases = [...send.cases, ...send.effects]
    .filter(row => [...(row.character?.triggerscript ?? []), ...moduleTriggers(row)].some(runsLua))
    .map(row => {
        const effect = row.outcome !== undefined
        // The send gives each nullish message ID of an effect row a fresh value before its start trigger runs.
        const rows = structuredClone(row.messages ?? (effect ? send.effectMessages : send.messages))
            .map((message, index) => (row.expectedIdAssignments ?? []).includes(index) ? { ...message, chatId: `assigned-${index}` } : message)
        const conversation = { ...send.conversation, ...row.conversation }
        const entry = {
            id: row.id,
            triggers: row.character?.triggerscript ?? send.character.triggerscript,
            lowLevelAccess: row.character?.lowLevelAccess ?? send.character.lowLevelAccess ?? false,
            modules: moduleTriggers(row),
            rows,
            scriptstate: structuredClone(conversation.scriptstate),
            note: conversation.note,
            ...(requests[row.id] ? { request: requests[row.id] } : {}),
        }
        beforeTrigger[row.id]?.(entry)
        return entry
    })

const ids = new Set(cases.map(entry => entry.id))
for (const id of [...Object.keys(beforeTrigger), ...Object.keys(requests)]) if (!ids.has(id)) throw new Error(`no Lua row ${id}`)
const text = `{"schema": 1, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`
if (text.includes('\r')) throw new Error('CR in cases')
writeFileSync(output, text)
console.log('trigger', cases.length)
