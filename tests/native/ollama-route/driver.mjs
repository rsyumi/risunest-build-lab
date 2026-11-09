import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { isDeepStrictEqual } from 'node:util'
import { setTimeout as delay } from 'node:timers/promises'
import { assertNoJsProcessing } from '../execution-profile/profile.mjs'
import { hasStatusAfterIssuedProof } from './commandWitness.mjs'
import { nativeGateCases, nativeEntryScenarios, verifyNativeGateOutcome, verifyStaleNativeToken, cancelAtNativeGate } from './nativeControls.mjs'

const fixtureSource = readFileSync(new URL('../../fixtures/native-generation/ollama-text.json', import.meta.url), 'utf8')
const fixtureData = JSON.parse(fixtureSource)
const sha256 = value => createHash('sha256').update(value).digest('hex')
const outputData = JSON.parse(readFileSync(new URL('../../fixtures/native-generation/ollama-owner-response.json', import.meta.url), 'utf8'))
const q = JSON.stringify
const reply = 'Synthetic native UI reply.'
const arrayCoercionDisplayedOutput = 'alpha ,7,nested,tail'
const input = 'Synthetic composer question.'
const edited = 'Synthetic edited question.'
const otherText = 'Synthetic other conversation.'
const characterId = 'character'
const conversationId = 'conversation'
const row = id => `[data-chat-id="${id}"]`
// A fallback chain can name providers this Ollama-only route never sends to. Its first entry is this same model, so the
// JS route sends one ordinary request to the fixture.
export const unsupportedRefusal = 'fallbackModels'
const unsupportedFallbackModels = { model: ['ollama-hosted'], memory: [], emotion: [], translate: [], otherAx: [] }
// Start trigger rows of the real TS Send oracle: two the route runs natively, one whose effect it refuses and one whose
// stop it refuses before reservation. `units` are the conversation units the native variable receipt must name.
const triggerScenarios = {
    'TRIGGER-VAR': { caseId: 'trigger-setvar-string', native: true, units: ['scriptstate'] },
    'TRIGGER-NOTE': { caseId: 'trigger-author-note', native: true, units: ['scriptstate', 'note'] },
    'TRIGGER-DECLINE': { caseId: 'trigger-modifychat', native: false },
    'TRIGGER-STOP': { caseId: 'trigger-stop', native: false },
}
export const scenarios = ['SUCC', 'SUCC-UNHELD', 'JS', 'JS-HISTORY', 'NATIVE-HISTORY', 'UNSUPPORTED', 'LORE', 'MODULE', 'LORE-MODULE',
    ...Object.keys(triggerScenarios), 'PERR-HTTP', 'PERR-A', 'PERR-JSON', 'PERR-NUMBER', 'CANB', 'CANA', 'CANA-BODY', 'NAV', 'EDIT', 'LOST-HOLD', 'LOST-COMMIT', 'DUP-SEND', 'DUP-RESTART', ...Object.keys(nativeGateCases)]
export const uncovered = [
    'Native gate observations cover only the selected buffered synthetic route, not all cancellation or streaming edges',
    'PERR: additional request/transform failures beyond HTTP, policy-A array coercion, malformed JSON and numeric content',
    'L31: complete processing-engine inventory and native stage classifications',
]

export async function until(read, accepts, label, timeoutMs = 45_000) {
    const end = Date.now() + timeoutMs
    for (;;) {
        const value = await read()
        if (accepts(value)) return value
        if (Date.now() >= end) throw new Error(`ollama-ui:timeout:${label}`)
        await delay(50)
    }
}

const loreCaseId = 'lore-global-local-state'
const moduleCaseId = 'module-selected-order'
const loreAssignments = ['__internal_ka_global', '__internal_da_local']
const moduleAssignments = ['__internal_ka_selected-b', '__internal_ka_selected-a', '__internal_ka_selected-c']

export function qualifyLoreOracle(oracle, source = fixtureSource, caseId = loreCaseId) {
    assert.ok(caseId === loreCaseId || caseId === moduleCaseId, 'Qualified lore/module oracle case')
    const assignments = caseId === moduleCaseId ? moduleAssignments : loreAssignments
    const fixtures = JSON.parse(source)
    assert.equal(oracle.schema, 1, 'Lore oracle schema')
    assert.equal(oracle.fixtureSha256, sha256(source), 'Lore oracle fixture hash')
    assert.deepEqual(oracle.cases.map(entry => entry.id), fixtures.cases.map(entry => entry.id), 'Lore oracle complete case identities')
    const sourceCase = fixtures.cases.find(entry => entry.id === caseId)
    const matches = oracle.cases.filter(entry => entry.id === caseId)
    assert.ok(sourceCase?.observeLore && matches.length === 1, 'One observed lore oracle case')
    const expected = matches[0]
    assert.equal(expected.output.text, sourceCase.expectedOutput, 'Lore oracle output')
    assert.deepEqual(JSON.parse(expected.body).messages, expected.prepared.messages, 'Lore oracle request messages')
    for (const field of ['requestScriptstate', 'storedScriptstate']) {
        const state = expected.loreObservation?.[field]
        assert.ok(state && !Array.isArray(state) && typeof state === 'object', `Lore oracle ${field}`)
        for (const name of assignments) assert.equal(state['$' + name], 'true', `Lore oracle completed ${name}`)
        if (caseId === moduleCaseId) assert.deepEqual(Object.keys(state).sort(), ['preserve', ...assignments.map(name => '$' + name)].sort(),
            'Module oracle has exactly the selected module writes')
        assert.equal(state.preserve, fixtures.conversation.scriptstate.preserve, 'Lore oracle preserves unrelated state')
    }
    assert.deepEqual(expected.loreObservation.assignments, assignments.map(name => ({ name, value: 'true' })),
        'Lore oracle ordered keep/dont assignments')
    const database = expected.database
    assert.equal(database.characters[0].chaId, characterId, 'Lore oracle character')
    assert.equal(database.characters[0].chats[0].id, conversationId, 'Lore oracle conversation')
    const messages = database.characters[0].chats[0].message
    assert.equal(messages.length, 3, 'Lore composer fixture history')
    assert.equal(messages.at(-1).role, 'user', 'Lore composer submits the last user row')
    return { expected: structuredClone(expected), responseContent: sourceCase.responseContent,
        input: messages.at(-1).data, fixtureSha256: oracle.fixtureSha256, caseId }
}

function readReviewedOracle(label, noun) {
    const path = process.env.RISUNEST_OLLAMA_PREPARATION_ORACLE
    assert.ok(path, `${label} scenarios require RISUNEST_OLLAMA_PREPARATION_ORACLE`)
    const expectedHash = process.env.RISUNEST_OLLAMA_PREPARATION_ORACLE_SHA256
    assert.match(expectedHash ?? '', /^[0-9a-f]{64}$/, `${label} scenarios require the reviewed oracle SHA-256`)
    const source = readFileSync(path, 'utf8')
    const oracleSha256 = sha256(source)
    assert.equal(oracleSha256, expectedHash, `Reviewed ${noun} oracle artifact hash`)
    return { oracle: JSON.parse(source), oracleSha256 }
}

function loadLoreOracle(caseId = loreCaseId) {
    const { oracle, oracleSha256 } = readReviewedOracle('LORE', 'lore')
    return { ...qualifyLoreOracle(oracle, fixtureSource, caseId), oracleSha256 }
}

const isRecord = value => value !== null && typeof value === 'object' && !Array.isArray(value)

/** Qualifies one start trigger row of the real TS Send oracle against the current fixture before any seed. */
export function qualifyTriggerOracle(oracle, source = fixtureSource, name) {
    const scenario = triggerScenarios[name]
    assert.ok(scenario, 'Qualified trigger oracle scenario')
    const fixtures = JSON.parse(source)
    assert.equal(oracle.schema, 1, 'Trigger oracle schema')
    assert.equal(oracle.fixtureSha256, sha256(source), 'Trigger oracle fixture hash')
    assert.deepEqual(oracle.cases.map(entry => entry.id), fixtures.cases.map(entry => entry.id), 'Trigger oracle complete case identities')
    assert.deepEqual(oracle.effects.map(entry => entry.id), fixtures.effects.map(entry => entry.id), 'Trigger oracle complete effect identities')
    // A stopping trigger is an effect row: it sends nothing, so it has no request or output to record.
    const stops = name === 'TRIGGER-STOP'
    const sourceCase = (stops ? fixtures.effects : fixtures.cases).find(entry => entry.id === scenario.caseId)
    const matches = (stops ? oracle.effects : oracle.cases).filter(entry => entry.id === scenario.caseId)
    assert.ok(sourceCase?.observeTriggers === true && matches.length === 1, 'One observed trigger oracle row')
    // The refusal is the one the fixture row names, which the Rust route tests also require.
    assert.equal(sourceCase.decline === undefined, scenario.native, 'Trigger oracle row runs natively or names its refusal')
    if (!scenario.native) assert.match(sourceCase.decline, /^[a-z][a-z.-]*$/, 'Trigger oracle refusal name')
    const expected = structuredClone(matches[0])
    const observation = expected.triggerObservation
    assert.ok(isRecord(observation) && observation.jsonUnsafe === undefined, 'Trigger oracle JSON-safe observation')
    assert.ok(isRecord(observation.storedScriptstate), 'Trigger oracle storedScriptstate')
    assert.equal(typeof observation.storedNote, 'string', 'Trigger oracle storedNote')
    const conversation = expected.database.characters[0].chats[0]
    assert.equal(expected.database.characters[0].chaId, characterId, 'Trigger oracle character')
    assert.equal(conversation.id, conversationId, 'Trigger oracle conversation')
    assert.notDeepEqual(observation.storedScriptstate, conversation.scriptstate, 'Trigger oracle records the start trigger writes')
    if (scenario.units?.includes('note')) assert.notEqual(observation.storedNote, conversation.note, 'Trigger oracle records the author note write')
    const identity = { expected, caseId: scenario.caseId, scenario: name, fixtureSha256: oracle.fixtureSha256,
        decline: sourceCase.decline ?? null, units: scenario.units ?? [] }
    if (stops) {
        assert.deepEqual([expected.outcome, expected.requests, expected.body, expected.error], ['stop', 0, null, null],
            'Trigger oracle stop row sends no request')
        assert.equal(observation.requestScriptstate, null, 'Trigger oracle stop row has no request variables')
        // The scenario seeds the ordinary synthetic history; the stored variables depend only on this starting map.
        assert.deepEqual(conversation.scriptstate, fixtures.conversation.scriptstate, 'Trigger oracle stop row starts from the base variables')
        return { ...identity, triggerscript: structuredClone(sourceCase.character.triggerscript) }
    }
    assert.ok(isRecord(observation.requestScriptstate), 'Trigger oracle requestScriptstate')
    assert.equal(expected.output.text, sourceCase.expectedOutput, 'Trigger oracle output')
    assert.deepEqual(JSON.parse(expected.body).messages, expected.prepared.messages, 'Trigger oracle request messages')
    assert.ok(Array.isArray(observation.storedMessages) && observation.storedMessages.length === 3, 'Trigger oracle stored history')
    assert.equal(conversation.message.length, 3, 'Trigger composer fixture history')
    assert.equal(conversation.message.at(-1).role, 'user', 'Trigger composer submits the last user row')
    return { ...identity, responseContent: sourceCase.responseContent, input: conversation.message.at(-1).data }
}

function loadTriggerOracle(name) {
    const { oracle, oracleSha256 } = readReviewedOracle(name, 'trigger')
    return { ...qualifyTriggerOracle(oracle, fixtureSource, name), oracleSha256 }
}

export function verifyLoreRequest(requests, lore, moduleText = null, label = 'Lore') {
    assert.equal(requests.length, 1, `${label} sends exactly one fixture request`)
    const request = requests[0]
    assert.equal(request.method, 'POST')
    assert.equal(request.url, '/api/chat')
    if (moduleText === null) assert.equal(request.text, lore.expected.body, `${label} request bytes match real TS Send`)
    else {
        assert.equal(request.json.model, JSON.parse(lore.expected.body).model)
        assert.equal(request.json.stream, false)
        assert.ok(request.json.messages.some(message => message.content.includes(moduleText)), 'Selected module reaches the JS request')
    }
}

export function verifyLoreModuleRows(rows, beforeRows, lore, label = 'Module fallback') {
    assert.equal(rows.length, beforeRows.length + 2, `${label} persists one composer row and one output`)
    assert.deepEqual(rows.slice(0, beforeRows.length), beforeRows, `${label} preserves exact ordered pre-Send rows`)
    const composer = rows[beforeRows.length]
    assert.equal(composer.role, 'user')
    assert.equal(composer.data, lore.input)
    assert.equal(rows.filter(value => value.data === lore.input).length, 1, `${label} has one composer submission`)
    const output = rows.at(-1)
    assert.equal(output.role, 'char')
    assert.equal(output.data, lore.expected.output.text)
    assert.equal(rows.filter(value => value.data === lore.expected.output.text).length, 1, `${label} has one persisted output`)
    return output
}

export function verifyLoreState(conversation, lore, phase) {
    const field = phase === 'issued' ? 'requestScriptstate' : 'storedScriptstate'
    assert.deepEqual(conversation.scriptstate, lore.expected.loreObservation[field], `Lore ${phase} full stored variable map`)
}

export function verifyLoreReceipts(proof, issuedProof) {
    verifyIssued(issuedProof)
    assert.ok(issuedProof.affectedUnitKeys.includes(JSON.stringify(['conversation', characterId, conversationId, 'scriptstate'])),
        'Lore pre-dispatch proof includes stored variable effects')
    assert.deepEqual(proof.receipts.effects, issuedProof.receipts.effects, 'Lore effect receipt identities survive completion')
}

/** The whole stored variable map and the stored author note at each phase; the note is written before dispatch. */
export function verifyTriggerState(conversation, trigger, phase) {
    const observation = trigger.expected.triggerObservation
    assert.deepEqual(conversation.scriptstate, observation[phase === 'issued' ? 'requestScriptstate' : 'storedScriptstate'],
        `Trigger ${phase} full stored variable map`)
    assert.equal(conversation.note, observation.storedNote, `Trigger ${phase} stored author note`)
}

const conversationUnit = unit => JSON.stringify(['conversation', characterId, conversationId, unit])

export function verifyTriggerReceipts(proof, issuedProof, units) {
    verifyIssued(issuedProof)
    assert.ok(units.length > 0, 'Trigger receipts name their conversation units')
    for (const unit of units) {
        assert.ok(issuedProof.affectedUnitKeys.includes(conversationUnit(unit)), `Trigger pre-dispatch proof includes the conversation ${unit} unit`)
    }
    assert.deepEqual(proof.receipts.effects, issuedProof.receipts.effects, 'Trigger effect receipt identities survive completion')
}

/** Earlier rows as the TS Send stored them (a trigger may edit them), one composer row and one output row. */
export function verifyTriggerRows(rows, trigger, label = 'Trigger Send') {
    const stored = trigger.expected.triggerObservation.storedMessages
    assert.equal(rows.length, stored.length + 1, `${label} persists one composer row and one output`)
    assert.deepEqual(rows.slice(0, stored.length - 1), stored.slice(0, -1), `${label} stores the earlier rows TS stored`)
    const composer = rows.at(-2)
    assert.deepEqual([composer.role, composer.data], ['user', trigger.input], `${label} stores the composer row`)
    assert.equal(rows.filter(value => value.data === trigger.input).length, 1, `${label} has one composer submission`)
    const output = rows.at(-1)
    assert.deepEqual([output.role, output.data], ['char', trigger.expected.output.text], `${label} stores the TS output`)
    return output
}

/** A JS Send that ran the start trigger itself. */
export function verifyJsStartTrigger(snapshot) {
    assert.ok(snapshot.counts['triggers.mode']['must-be-native'] > 0, 'The JS route ran the start trigger')
}

/** The JS route ran the stopping start trigger and ended the Send before any request or response handling. */
export function verifyTriggerStopControl(snapshot, manifest) {
    const zero = structuredClone(snapshot)
    for (const counts of Object.values(zero.counts)) {
        assert.ok(Object.values(counts).every(value => Number.isSafeInteger(value) && value >= 0), 'Valid trigger stop counts')
        counts['must-be-native'] = 0; counts['web-only'] = 0
    }
    assertNoJsProcessing(zero, manifest)
    assert.ok(snapshot.counts['send.preparation']['must-be-native'] > 0, 'The stopped Send entered JS preparation')
    verifyJsStartTrigger(snapshot)
    for (const id of ['request.wrapper', 'request.main', 'request.ollama', 'response.application']) {
        assert.equal(snapshot.counts[id]['must-be-native'], 0, `A stopping start trigger makes no request: ${id}`)
    }
}

export function verifyTriggerStopRows(rows, beforeRows, text = input) {
    assert.equal(rows.length, beforeRows.length + 1, 'A stopped Send keeps only the composer row')
    assert.deepEqual(rows.slice(0, beforeRows.length), beforeRows, 'A stopped Send preserves exact ordered earlier rows')
    assert.deepEqual([rows.at(-1).role, rows.at(-1).data], ['user', text], 'A stopped Send stores the composer row')
}

export class UiDriver {
    constructor(page, manifest, diagnostic = () => {}) { this.page = page; this.manifest = manifest; this.diagnostic = diagnostic }
    evaluate(expression) { return this.page.evaluate(expression) }
    invoke(command, args = {}) {
        return this.evaluate(`globalThis.__risunestOllamaUi.witness.inspect(${q(command)}, ${q(args)})`)
    }
    async seed(origin, enabled, unsupported, historyLimit = false, lore = null, triggerscript = null) {
        this.diagnostic({ event: 'seed-started' })
        await this.invoke('native_startup_status')
        this.diagnostic({ event: 'native-startup-ready' })
        const opened = await this.invoke('pds_open')
        assert.equal(opened.revision, 0, 'Only a fresh store can be seeded')
        this.diagnostic({ event: 'fresh-store-confirmed' })
        const base = structuredClone(fixtureData)
        if (triggerscript) base.character.triggerscript = structuredClone(triggerscript)
        if (lore) {
            const database = structuredClone(lore.expected.database)
            base.preset = database.botPresets[0]
            base.character = database.characters[0]
            base.conversation = base.character.chats[0]
            base.root = database
            delete base.root.botPresets
            delete base.root.characters
            if (lore.module) {
                base.root.modules = [structuredClone(lore.module)]
                base.root.enabledModules = [lore.module.id]
            }
        }
        // The route reads the selected preset's fallback chain, and the loaded root carries the same value.
        if (unsupported) base.preset.fallbackModels = structuredClone(unsupportedFallbackModels)
        const root = { ...base.root, ...(lore ? {} : base.preset),
            botPresetsId: base.preset.id, selectedPersona: lore ? base.root.selectedPersona : base.root.personas[0].id,
            characterOrder: [base.character.chaId],
            language: 'en', theme: '', didFirstSetup: true, ollamaURL: origin,
            risunestChatEditPopup: false, showMenuChatList: true, requestRetrys: 0,
        }
        delete root.id
        delete root.name
        const character = { ...base.character, chats: [
            { ...base.conversation, message: lore ? base.conversation.message.slice(0, -1) : [
                { role: 'user', data: 'Synthetic earlier question.', chatId: 'message-a' },
                { role: 'char', data: 'Synthetic earlier answer.', chatId: 'message-b' },
            ] },
            { ...base.conversation, id: 'conversation-other', name: 'Synthetic other',
                message: [{ role: 'user', data: otherText, chatId: 'message-other' }] },
        ] }
        const { stagingId } = await this.invoke('pds_replace_begin')
        await this.invoke('pds_replace_put_root', { stagingId, root })
        await this.invoke('pds_replace_put_presets', { stagingId, presets: [base.preset] })
        await this.invoke('pds_replace_add_characters', { stagingId, characters: [character] })
        await this.invoke('pds_replace_commit', { stagingId, expectedRevision: 0 })
        this.diagnostic({ event: 'synthetic-seed-committed' })
        await this.invoke('pds_set_device_setting', { key: 'risuNestNativeOllamaGeneration', value: String(enabled) })
        this.diagnostic({ event: 'native-route-setting-saved' })
        if (historyLimit) {
            await this.invoke('pds_set_device_setting', { key: 'risuNestDeviceSettings', value: JSON.stringify({
                schema: 'risunest.device-settings/v1', performanceProfile: 'normal',
                androidKeepAliveDuringGeneration: true, nativeFileLogEnabled: true,
                generationHistoryLimitEnabled: true, generationHistoryLimitMultiplier: 2,
                messageSendKey: 'enter', pluginOutputCurrentChatOnly: false,
            }) })
        }
        await this.invoke('pds_set_device_setting', { key: 'risuNestDefaultPluginsAttempted', value: JSON.stringify(['provider-manager', 'yumi-translator']) })
    }
    async captureStartupTimeout() {
        try {
            const dom = await this.evaluate(`(() => {
                const visible = element => element.getClientRects().length > 0
                const selectors = ['[data-char-id="character"]', '[data-char-id]', 'textarea.text-input-area',
                    '.button-icon-send', '.z-modal', '[role="alert"]', '[aria-busy="true"]', '.animate-spin', '[class*="loading"]', '[class*="error"]']
                const counts = Object.fromEntries(selectors.map(selector => [selector, document.querySelectorAll(selector).length]))
                const summaries = selector => [...document.querySelectorAll(selector)].filter(visible).slice(0, 8).map(element => ({
                    tag: element.tagName, role: element.getAttribute('role'),
                    text: element.textContent.replace(/\\s+/g, ' ').trim().slice(0, 384),
                }))
                return { readyState: document.readyState, totalElements: document.querySelectorAll('*').length,
                    counts, visibleModals: summaries('.z-modal'), visibleAlerts: summaries('[role="alert"]'),
                    visibleLoading: summaries('[aria-busy="true"], .animate-spin, [class*="loading"]'),
                    visibleErrors: summaries('[class*="error"]'),
                    structure: [...document.body.querySelectorAll('*')].slice(0, 80).map(element => ({
                        tag: element.tagName, id: element.id.slice(0, 80),
                        classes: [...element.classList].slice(0, 6).map(value => value.slice(0, 64)), visible: visible(element),
                    })) }
            })()`)
            this.diagnostic({ event: 'product-sidebar-timeout', dom })
        } catch (error) {
            this.diagnostic({ event: 'startup-dom-capture-failed', error: String(error).slice(0, 256) })
        }
    }
    async startApp() {
        this.diagnostic({ event: 'startup-release-requested' })
        await this.evaluate('globalThis.__risunestOllamaUi.release()')
        this.diagnostic({ event: 'startup-released' })
        try {
            await until(() => this.evaluate(`!!document.querySelector('[data-char-id="character"]')`),
                Boolean, 'product sidebar')
        } catch (error) {
            if (error?.message === 'ollama-ui:timeout:product sidebar') await this.captureStartupTimeout()
            throw error
        }
        this.diagnostic({ event: 'product-sidebar-ready' })
        const ownTermsAccepted = () => this.evaluate(`globalThis.__TAURI_INTERNALS__
            .invoke('pds_get_device_setting', { key: 'risunest_tos_v1' }).then(value => value === 'true')`)
        const alreadyAccepted = await ownTermsAccepted()
        this.diagnostic({ event: alreadyAccepted ? 'own-terms-already-accepted' : 'own-terms-required' })
        const ownTerms = `[...document.querySelectorAll('.z-modal')].filter(element =>
            element.getClientRects().length && element.textContent.includes('To use RisuNest, accept')
            && element.textContent.includes('Terms of Use') && element.textContent.includes('Privacy Notice'))`
        if (!alreadyAccepted) {
            this.diagnostic({ event: 'own-terms-dialog-awaited' })
            await until(() => this.evaluate(`(() => {
                const own = ${ownTerms}
                if (own.length !== 1) return false
                const buttons = [...own[0].querySelectorAll('button')].filter(button =>
                    button.getClientRects().length && !button.disabled && button.textContent.trim() === 'Accept')
                if (buttons.length !== 1) return false
                buttons[0].click()
                return true
            })()`), Boolean, 'unique own RisuNest Terms acceptance')
            this.diagnostic({ event: 'own-terms-accept-clicked' })
        }
        await until(async () => {
            if (!await ownTermsAccepted()) return false
            return this.evaluate(`(${ownTerms}).length === 0`)
        }, Boolean, 'persisted own RisuNest Terms acceptance')
        this.diagnostic({ event: 'own-terms-persisted-and-absent' })
        await this.click('[data-char-id="character"]')
        this.diagnostic({ event: 'synthetic-character-selected' })
        await this.idle()
        this.diagnostic({ event: 'send-ready' })
    }
    async click(selector) {
        await until(() => this.evaluate(`(() => {
            const items = [...document.querySelectorAll(${q(selector)})].filter(e => e.getClientRects().length && !e.disabled)
            if (items.length !== 1) return false
            items[0].scrollIntoView({block:'center'})
            items[0].click()
            return true
        })()`), Boolean, 'unique enabled control')
    }
    async fill(selector, text) {
        await until(() => this.evaluate(`(() => {
            const items = [...document.querySelectorAll(${q(selector)})].filter(e => e.getClientRects().length && !e.disabled)
            if (items.length !== 1) return false
            const element = items[0]
            element.focus()
            const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set
            setter.call(element, ${q(text)})
            element.dispatchEvent(new Event('input', {bubbles:true}))
            element.dispatchEvent(new Event('change', {bubbles:true}))
            return true
        })()`), Boolean, 'editable textarea')
    }
    idle() {
        return until(() => this.evaluate(`!!document.querySelector('textarea.text-input-area') &&
            !!document.querySelector('.button-icon-send:not(:disabled)') &&
            !document.querySelector('button[aria-labelledby="cancel"]')`), Boolean, 'Send ready')
    }
    async begin() {
        assert.equal(await this.evaluate('globalThis.__risunestExecutionProfile.identity'), this.manifest.identity)
        await this.evaluate(`globalThis.__risunestExecutionProfile.begin(${q(this.manifest.identity)});
            globalThis.__risunestOllamaUi.witness.begin()`)
    }
    profile(end = false) {
        return this.evaluate(`globalThis.__risunestExecutionProfile.${end ? 'end' : 'snapshot'}(${q(this.manifest.identity)})`)
    }
    witness() { return this.evaluate('globalThis.__risunestOllamaUi.witness.snapshot()') }
    arm(barrier) { return this.evaluate(`globalThis.__risunestOllamaUi.witness.arm(${q(barrier)})`) }
    barrier(name) { return until(() => this.witness(), value => value.blocked === name, name) }
    resume() { return this.evaluate('globalThis.__risunestOllamaUi.witness.resume()') }
    async send(double = false, text = input) {
        await this.fill('textarea.text-input-area', text)
        if (!double) return this.click('.button-icon-send')
        await this.evaluate(`(() => {
            const button = document.querySelector('.button-icon-send')
            if (!button || button.disabled) throw new Error('ollama-ui:Send-unavailable')
            button.click(); button.click()
        })()`)
    }
    stop() { return this.click('button[aria-labelledby="cancel"]') }
    async ticket() {
        const witness = await until(() => this.witness(), value => value.events.some(event =>
            event.command === 'native_generation_reserve_ollama' && event.result?.ticket), 'product reservation')
        return witness.events.find(event => event.command === 'native_generation_reserve_ollama' && event.result?.ticket).result.ticket
    }
    status(ticket) { return this.invoke('native_generation_status', { ticket, waitMs: 0 }) }
    settled(ticket) {
        return until(() => this.status(ticket), value => !['reserved', 'running'].includes(value.phase) && !value.pendingCommit,
            'terminal native status')
    }
    conversation(id = conversationId) {
        return this.invoke('pds_read_conversation', { characterId, conversationId: id, lease: null }).then(result => result.value)
    }
    messages(id = conversationId) { return this.conversation(id).then(value => value.message) }
    async shown(id, text) {
        try {
            await until(() => this.evaluate(`(() => {
                const rows = [...document.querySelectorAll(${q(row(id))})].filter(e => e.getClientRects().length)
                return rows.length === 1 && rows[0].innerText.includes(${q(text)})
            })()`), Boolean, 'visible output row')
        } catch (error) {
            if (error?.message === 'ollama-ui:timeout:visible output row') {
                let evidence = { captureSkipped: 'closed-page' }
                try {
                    if (this.page.isOpen()) {
                        const response = await this.page.send('Runtime.evaluate', { expression: `(() => {
                            const rows = [...document.querySelectorAll(${q(row(id))})]
                            return { matchingRows: rows.length, visibleRows: rows.filter(element => element.getClientRects().length).length,
                                omittedRows: Math.max(0, rows.length - 4), rows: rows.slice(0, 4).map(element => ({
                                    hasLayout: element.getClientRects().length > 0,
                                    text: element.innerText.replace(/\\s+/g, ' ').trim().slice(0, 512),
                                })) }
                        })()`, awaitPromise: false, returnByValue: true }, 3_000)
                        evidence = response.exceptionDetails || !response.result?.value
                            ? { captureFailed: true } : response.result.value
                    }
                } catch { evidence = { captureFailed: true } }
                try { console.log('Visible output row timeout evidence: ' + JSON.stringify(evidence)) } catch {}
            }
            throw error
        }
    }
    errorDialogs() {
        return this.evaluate(`[...document.querySelectorAll('.z-modal h2')].filter(e => e.getClientRects().length
            && e.textContent === 'Error').map(e => e.parentElement.innerText.replace(/\\s+/g, ' ').trim().slice(0, 256))`)
    }
    async error(text) {
        await until(() => this.evaluate(`[...document.querySelectorAll('.z-modal h2')].some(e =>
            e.getClientRects().length && e.textContent === 'Error' &&
            e.parentElement.innerText.includes(${q(text)}))`), Boolean, 'visible error dialog')
    }
    async captureFailure(fixture) {
        const response = await this.page.send('Runtime.evaluate', { expression: `(() => {
            const evidence = {}
            const scalar = value => typeof value === 'string' ? value.slice(0, 96)
                : typeof value === 'number' && Number.isFinite(value) || typeof value === 'boolean' ? value : null
            try {
                const witness = globalThis.__risunestOllamaUi.witness.snapshot()
                const events = witness.events.length > 32
                    ? [...witness.events.slice(0, 16), ...witness.events.slice(-16)] : witness.events
                evidence.commands = events.map(event => ({
                    command: String(event.command).slice(0, 96), state: String(event.state).slice(0, 32),
                    invocationOrder: scalar(event.invocationOrder), completionOrder: scalar(event.completionOrder),
                    phase: scalar(event.result?.phase ?? event.result?.at), dispatch: scalar(event.result?.dispatch),
                    output: scalar(event.result?.output), checkpoint: scalar(event.result?.checkpoint),
                    unsupported: scalar(event.result?.unsupported),
                }))
                evidence.omittedCommands = Math.max(0, witness.events.length - 32)
            } catch { evidence.commandCaptureFailed = true }
            try {
                const snapshot = globalThis.__risunestExecutionProfile.snapshot(${q(this.manifest.identity)})
                const ids = ['send.preparation', 'send.history-window', 'request.wrapper', 'request.main', 'request.ollama',
                    'response.application', 'tokenizer.encode', 'tokenizer.explicit', 'tokenizer.chat', 'tokenizer.chats']
                evidence.counters = Object.fromEntries(ids.map(id => {
                    const value = snapshot.counts[id]?.['must-be-native']
                    return [id, Number.isFinite(value) ? value : null]
                }))
            } catch { evidence.counterCaptureFailed = true }
            try {
                const visible = element => element.getClientRects().length > 0
                const available = selector => {
                    const elements = [...document.querySelectorAll(selector)].filter(visible)
                    return { visible: elements.length, enabled: elements.filter(element => !element.disabled).length }
                }
                evidence.dom = { modals: [...document.querySelectorAll('.z-modal')].filter(visible).slice(0, 6).map(element => ({
                    heading: (element.querySelector('h2')?.textContent ?? '').trim().slice(0, 128),
                    message: element.innerText.replace(/\\s+/g, ' ').trim().slice(0, 384),
                })), send: available('.button-icon-send'), cancel: available('button[aria-labelledby="cancel"]') }
            } catch { evidence.domCaptureFailed = true }
            return evidence
        })()`, awaitPromise: false, returnByValue: true }, 3_000)
        if (response.exceptionDetails || !response.result?.value) throw new Error('ollama-ui:failure-capture-unavailable')
        return { ...response.result.value, fixtureRequests: Number.isFinite(fixture?.requestCount) ? fixture.requestCount : null }
    }
    async navigate(name) {
        await this.click('.button-icon-send + button, button[aria-labelledby="cancel"] + button')
        await until(() => this.evaluate(`(() => {
            const spans = [...document.querySelectorAll('span')].filter(e => e.getClientRects().length && e.textContent === 'Chat List')
            if (spans.length !== 1) return false
            spans[0].parentElement.click()
            return true
        })()`), Boolean, 'Chat List menu')
        await until(() => this.evaluate(`(() => {
            const headings = [...document.querySelectorAll('h2')].filter(e => e.getClientRects().length && e.textContent === 'Chat List')
            if (headings.length !== 1) return false
            const panel = headings[0].parentElement.parentElement
            const buttons = [...panel.querySelectorAll('button')].filter(e => e.textContent.trim() === ${q(name)})
            if (buttons.length !== 1) return false
            buttons[0].click()
            return true
        })()`), Boolean, 'conversation selection')
    }
    async editEarlier() {
        await this.click(`${row('message-a')} .button-icon-edit`)
        await this.fill(`${row('message-a')} textarea`, edited)
        await this.click(`${row('message-a')} .button-icon-edit`)
    }
}

function verifyNativeEvents(witness, { execute = true } = {}) {
    for (const command of ['native_generation_host_init', 'native_generation_reserve_ollama']) {
        assert.ok(witness.events.some(event => event.command === command && event.state === 'resolved'), command)
    }
    const executes = witness.events.filter(event => event.command === 'native_generation_execute')
    assert.equal(executes.length, execute ? 1 : 0, 'Only the real product Send issues execute')
    if (execute) assert.equal(executes[0].state, 'resolved')
}
const sameTicket = (actual, expected) => actual?.epoch === expected.epoch && actual?.sequence === expected.sequence
const executes = witness => witness.events.filter(event => event.command === 'native_generation_execute')

export function verifyTerminalWitness(witness, ticket, phase, { requireStatus = true } = {}) {
    if (requireStatus) assert.ok(witness.events.some(event => event.command === 'native_generation_status'
        && event.state === 'resolved' && sameTicket(event.result?.ticket, ticket)
        && event.result.phase === phase), 'Real product observes this ticket terminal status')
    assert.ok(witness.events.some(event => event.command === 'native_generation_proof'
        && event.state === 'resolved' && sameTicket(event.result?.ticket, ticket)
        && event.result.at === phase), 'Real product receives this ticket terminal adoption proof')
}

export function verifyRecoveryWitness(before, recovered, ticket, phase, { committed = false, beforeRows, recoveredRows } = {}) {
    verifyNativeEvents(before)
    assert.equal(executes(recovered).length, 0, 'Recovery never executes generation')
    assert.equal(executes(before).length + executes(recovered).length, 1, 'One execute across renderer loss')
    if (committed) {
        // The pre-loss proof is withheld from its caller. Startup loads the committed projection directly.
        assert.ok(recovered.events.some(event => event.command === 'native_generation_active'
            && event.state === 'resolved' && Array.isArray(event.result)
            && !event.result.some(record => sameTicket(record.ticket, ticket))), 'Startup observes the committed ticket is inactive')
        assert.ok(Array.isArray(beforeRows) && Array.isArray(recoveredRows), 'Committed snapshot rows are captured')
        assert.deepEqual(recoveredRows, beforeRows, 'Full ordered rows survive committed snapshot initialization')
        return 'committed snapshot initialization'
    }
    verifyTerminalWitness(recovered, ticket, phase)
    return 'active-ticket recovery'
}

export async function verifyRestart(driver, manifest, sendSnapshot, checkRows) {
    await driver.begin()
    await driver.startApp()
    await checkRows(driver)
    const snapshot = await driver.profile(true)
    assert.equal(snapshot.counts['send.entry']['allowed-webview'], 0, 'Restart never enters Send')
    assertNoJsProcessing(sendSnapshot, manifest)
    for (const counts of Object.values(snapshot.counts)) {
        assert.ok(Object.values(counts).every(value => Number.isSafeInteger(value) && value >= 0), 'Valid restart counts')
    }
    // Keep the restart identity and combine only actual counts from the original Send and restart.
    const combined = structuredClone(snapshot)
    for (const [id, counts] of Object.entries(sendSnapshot.counts)) {
        for (const [engine, value] of Object.entries(counts)) combined.counts[id][engine] += value
    }
    assertNoJsProcessing(combined, manifest)
    const witness = await driver.witness()
    assert.equal(executes(witness).length, 0, 'Restart never executes generation')
    return { snapshot, witness }
}

/** An owner that commits during the product's Issued adoption still completes, without a renderer cancellation. */
export function verifyUnheldCompletion(witness, status) {
    const cancels = witness.events.filter(event => event.command === 'native_generation_cancel')
    assert.equal(cancels.length, 0, `The product cancelled an unheld native completion (${cancels.map(event =>
        `${event.state} ${event.result?.phase}/${event.result?.cancelDecision}`).join(', ')})`)
    assert.equal(status.phase, 'completed', 'An unheld native reply completes')
}

function verifyIssued(proof) {
    assert.equal(proof.at, 'issued')
    assert.equal(proof.receipts.output, null)
    assert.equal(proof.outputMessageId, null)
    assert.ok(proof.receipts.effects.length > 0, 'Verified pre-dispatch effect receipts')
    for (const key of [['root', 'statics'], ['character', characterId, 'lastInteraction'],
        ['conversation', characterId, conversationId, 'lastMemory']]) assert.ok(proof.affectedUnitKeys.includes(JSON.stringify(key)))
}
export function verifyJsControl(snapshot, manifest, { historyLimit = false } = {}) {
    // Reuse all observer identity/shape checks with a copy whose processing entries are zero.
    const zero = structuredClone(snapshot)
    for (const counts of Object.values(zero.counts)) {
        assert.ok(Object.values(counts).every(value => Number.isSafeInteger(value) && value >= 0), 'Valid JS control counts')
        counts['must-be-native'] = 0; counts['web-only'] = 0
    }
    assertNoJsProcessing(zero, manifest)
    const counterIds = ['send.preparation', 'send.history-window', 'request.wrapper', 'request.main', 'request.ollama',
        'response.application', 'tokenizer.encode', 'tokenizer.explicit', 'tokenizer.chat', 'tokenizer.chats']
    console.log('JS control counters: ' + JSON.stringify(Object.fromEntries(
        counterIds.map(id => [id, snapshot.counts[id]['must-be-native']]))))
    if (historyLimit) assert.ok(snapshot.counts['send.history-window']['must-be-native'] > 0, 'JS history-window positive control')
    else assert.equal(snapshot.counts['send.history-window']['must-be-native'], 0, 'JS fixture leaves generation history limit disabled')
    for (const id of ['send.preparation', 'request.wrapper', 'request.main', 'request.ollama', 'response.application']) {
        assert.ok(snapshot.counts[id]['must-be-native'] > 0, `JS positive control: ${id}`)
    }
    assert.ok(['tokenizer.encode', 'tokenizer.explicit', 'tokenizer.chat', 'tokenizer.chats']
        .some(id => snapshot.counts[id]['must-be-native'] > 0), 'JS tokenizer positive control')
}

const isJsControl = name => name === 'JS' || name === 'JS-HISTORY'
// A send whose history window applies stays on the JS route even with native opt-in on.
const takesJsRoute = name => isJsControl(name) || name === 'NATIVE-HISTORY'
const hasHistoryLimit = name => name === 'JS-HISTORY' || name === 'NATIVE-HISTORY'

export async function startScenario(driver, name, origin) {
    assert.ok(scenarios.includes(name))
    if (Object.hasOwn(triggerScenarios, name)) {
        driver.trigger = loadTriggerOracle(name)
        // The stop row is an effect row with its own history; its trigger is seeded over the ordinary synthetic history.
        if (driver.trigger.triggerscript) await driver.seed(origin, true, false, false, null, driver.trigger.triggerscript)
        else await driver.seed(origin, true, false, false, driver.trigger)
    } else if (name === 'LORE' || name === 'MODULE' || name === 'LORE-MODULE') {
        driver.lore = loadLoreOracle(name === 'MODULE' ? moduleCaseId : loreCaseId)
        if (name === 'LORE-MODULE') {
            const entry = fixtureData.cases.find(value => value.id === 'lore-inactive-nonempty')
            const module = entry?.root?.modules?.find(value => value.id === 'unselected-lore-module')
            assert.ok(module?.lorebook?.length === 1, 'Selected-module control fixture')
            driver.lore.module = { ...structuredClone(module), customModuleToggle: 'synthetic=Synthetic module caption=caption' }
        }
        await driver.seed(origin, true, false, false, driver.lore)
    } else await driver.seed(origin, !isJsControl(name), name === 'UNSUPPORTED', hasHistoryLimit(name))
    await driver.startApp()
}

/** The seeded trigger must still be refused, or the case would silently run the native route. */
export function verifyUnsupportedReservation(witness, refusal = unsupportedRefusal, label = 'UNSUPPORTED') {
    const reservations = witness.events.filter(event => event.command === 'native_generation_reserve_ollama')
    const outcome = reservations[0]?.state !== 'resolved' ? `a ${reservations[0]?.state ?? 'missing'} reservation`
        : reservations[0].result?.ticket ? 'a reserved ticket' : `refusal ${reservations[0].result?.unsupported}`
    assert.ok(reservations.length === 1 && reservations[0].state === 'resolved'
        && reservations[0].result?.unsupported === refusal && reservations[0].result?.ticket == null,
        `Stale ${label} trigger: expected refusal ${refusal}, observed ${outcome}; seed a refusal the route still has`)
}

export async function verifyJsRestart(driver, manifest, sendSnapshot, rows, output) {
    verifyJsControl(sendSnapshot, manifest, { historyLimit: true })
    await driver.begin()
    await driver.startApp()
    assert.deepEqual(await driver.messages(), rows, 'JS history rows survive actual app restart')
    await driver.shown(output.chatId, output.data)
    const snapshot = await driver.profile(true)
    assert.equal(snapshot.counts['send.entry']['allowed-webview'], 0, 'JS restart never enters Send')
    for (const counts of Object.values(snapshot.counts)) {
        assert.ok(Object.values(counts).every(value => Number.isSafeInteger(value) && value >= 0), 'Valid JS restart counts')
        assert.equal(counts['must-be-native'], 0, 'JS restart performs no generation processing')
        assert.equal(counts['web-only'], 0, 'JS restart performs no web-only processing')
    }
    // Preserve the restart identity and validate the actual Send and restart observations together.
    const combined = structuredClone(snapshot)
    for (const [id, counts] of Object.entries(sendSnapshot.counts)) {
        for (const [engine, value] of Object.entries(counts)) combined.counts[id][engine] += value
    }
    verifyJsControl(combined, manifest, { historyLimit: true })
    const witness = await driver.witness()
    assert.ok(witness.events.length > 0, 'JS restart observes an empty native active scan')
    assert.ok(witness.events.every(event => event.command === 'native_generation_active'
        && event.state === 'resolved' && Array.isArray(event.result) && event.result.length === 0),
        'JS restart permits only empty native active scans')
    return { snapshot, witness, rowsVerified: true }
}

const wireCases = ['CANA-BODY', 'PERR-JSON', 'PERR-NUMBER']

export async function cancelHeldBody(driver, fixture, ticket) {
    await fixture.waitForBodyStart(1)
    const bodyStart = { bodyStartedCount: fixture.bodyStartedCount, heldCount: fixture.heldCount }
    assert.deepEqual(bodyStart, { bodyStartedCount: 1, heldCount: 1 }, 'A nonempty partial body is held before Stop')
    await driver.stop()
    const status = await driver.settled(ticket)
    assert.equal(status.phase, 'cancelled', 'Body cancellation settles before release')
    assert.equal(status.cancelDecision, 'user')
    assert.equal(status.dispatch, 'issued')
    assert.equal(status.checkpoint, '0')
    assert.equal(status.output, 'none')
    await until(() => fixture.abortedCount, count => count === 1, 'cancelled body connection closed')
    fixture.release()
    return { bodyStart, status, abortedCount: fixture.abortedCount }
}

export function verifyWireFailure(name, status) {
    const expected = name === 'PERR-JSON' ? ['decode', 'syntax', 'buffered-json'] : ['type', 'indexOf', 'transform']
    assert.ok(['PERR-JSON', 'PERR-NUMBER'].includes(name))
    const failure = status.failure
    assert.deepEqual([failure?.category, failure?.field, failure?.stage], expected, 'Native response failure category, field and stage')
    assert.equal(typeof failure.sdkMessage, 'string')
    assert.ok(failure.sdkMessage.trim().length > 0, 'Native response failure has a visible error')
    return failure.sdkMessage
}

export function verifyWireProof(name, status, proof, issuedProof) {
    assert.ok(wireCases.includes(name))
    assert.equal(status.phase, name === 'CANA-BODY' ? 'cancelled' : 'failed')
    assert.equal(status.dispatch, 'issued')
    assert.equal(status.checkpoint, '0')
    assert.equal(status.output, 'none')
    if (name === 'CANA-BODY') assert.equal(status.cancelDecision, 'user')
    assert.equal(proof.at, status.phase)
    assert.deepEqual(proof.ticket, status.ticket)
    assert.equal(proof.receipts.output, null)
    assert.equal(proof.outputMessageId, null)
    assert.equal(proof.failedWithOutput, false)
    assert.deepEqual(proof.receipts.effects, issuedProof.receipts.effects, 'Pre-dispatch effects survive the wire failure or cancellation')
}

async function runLoreScenario({ name, driver, fixture, manifest, restart }) {
    const lore = driver.lore
    assert.ok(lore, 'Lore startup qualified the real TS oracle')
    fixture.setReply({ content: lore.responseContent })
    if (name !== 'LORE-MODULE') fixture.hold()
    const beforeRows = name === 'LORE-MODULE' || name === 'MODULE' ? await driver.messages() : null
    await driver.begin()
    await driver.send(false, lore.input)
    const oracleIdentity = { fixtureSha256: lore.fixtureSha256, oracleSha256: lore.oracleSha256, caseId: lore.caseId }
    if (name === 'LORE-MODULE') {
        const refused = await until(() => driver.witness(), value => value.events.some(event =>
            event.command === 'native_generation_reserve_ollama' && event.state !== 'pending'), 'selected module reservation outcome')
        verifyUnsupportedReservation(refused, 'modules', name)
        await fixture.waitForRequests(1)
        await driver.idle()
        const witness = await driver.witness()
        verifyUnsupportedReservation(witness, 'modules', name)
        verifyNativeEvents(witness, { execute: false })
        assert.ok(!witness.events.some(event => ['native_generation_capture_witness', 'native_generation_proof'].includes(event.command)),
            'A selected module has no native preparation witness or proof')
        const snapshot = await driver.profile(true)
        verifyJsControl(snapshot, manifest)
        verifyLoreRequest(fixture.requests, lore, lore.module.lorebook[0].content)
        const stored = await driver.conversation()
        verifyLoreState(stored, lore, 'terminal')
        const output = verifyLoreModuleRows(stored.message, beforeRows, lore)
        await driver.shown(output.chatId, output.data)
        return { name, passed: true, oracleIdentity, witness, snapshot, requests: fixture.requestCount,
            storedScriptstate: stored.scriptstate, beforeRows, rows: stored.message, output }
    }
    const ticket = await driver.ticket()
    await fixture.waitForRequests(1)
    await until(() => driver.status(ticket), value => value.dispatch === 'issued', 'Lore Issued')
    const issuedProof = await driver.invoke('native_generation_proof', { ticket, normalization: null })
    verifyIssued(issuedProof)
    await until(() => driver.witness(), hasStatusAfterIssuedProof, 'Lore product Issued adoption')
    const issuedConversation = await driver.conversation()
    verifyLoreState(issuedConversation, lore, 'issued')
    verifyLoreRequest(fixture.requests, lore)
    assertNoJsProcessing(await driver.profile(), manifest)
    fixture.release()
    const status = await driver.settled(ticket)
    assert.equal(status.phase, 'completed')
    assert.equal(status.output, 'committed')
    await driver.idle()
    assert.deepEqual(await driver.errorDialogs(), [], 'Lore native Send shows no error dialog')
    const witness = await driver.witness()
    verifyNativeEvents(witness)
    verifyTerminalWitness(witness, ticket, 'completed')
    const snapshot = await driver.profile(true)
    assertNoJsProcessing(snapshot, manifest)
    const proof = await driver.invoke('native_generation_proof', { ticket, normalization: null })
    verifyLoreReceipts(proof, issuedProof)
    assert.ok(proof.receipts.output)
    assert.equal(proof.outputMessageId, status.messageId)
    assert.equal(proof.failedWithOutput, false)
    const terminal = await driver.conversation()
    verifyLoreState(terminal, lore, 'terminal')
    if (name === 'MODULE') verifyLoreModuleRows(terminal.message, beforeRows, lore, 'Native module Send')
    assert.deepEqual(terminal.message.slice(0, -1), issuedConversation.message, 'Lore output preserves all submitted rows')
    assert.equal(terminal.message.filter(value => value.data === lore.input).length, 1, 'One lore composer submission')
    assert.equal(terminal.message.at(-1).chatId, status.messageId)
    assert.equal(terminal.message.at(-1).data, lore.expected.output.text)
    await driver.shown(status.messageId, lore.expected.output.text)
    let restartedConversation
    const restartEvidence = await verifyRestart(await restart(driver), manifest, snapshot, async current => {
        restartedConversation = await current.conversation()
        assert.deepEqual(restartedConversation.message, terminal.message, 'Lore ordered rows survive actual restart')
        verifyLoreState(restartedConversation, lore, 'restart')
        await current.shown(status.messageId, lore.expected.output.text)
    })
    assert.equal(fixture.requestCount, 1, 'Lore restart never redispatches')
    return { name, passed: true, oracleIdentity, ticket, status, issuedProof, proof, witness, snapshot,
        requests: fixture.requestCount, restartEvidence, restartVerified: true,
        storedMaps: { issued: issuedConversation.scriptstate, terminal: terminal.scriptstate, restart: restartedConversation.scriptstate },
        request: fixture.requests[0].json,
        moduleRows: name === 'MODULE' ? { beforeSend: beforeRows, issued: issuedConversation.message,
            terminal: terminal.message, restart: restartedConversation.message } : null }
}

// Waits for stored data to reach an expected shape, then returns the last read so a strict assertion reports any difference.
async function settledRead(read, accepts, label) {
    try { return await until(read, accepts, label) }
    catch (error) {
        if (error?.message !== `ollama-ui:timeout:${label}`) throw error
        return read()
    }
}

const reservationOutcome = (driver, label) => until(() => driver.witness(), value => value.events.some(event =>
    event.command === 'native_generation_reserve_ollama' && event.state !== 'pending'), label)

function verifyRefusedTriggerWitness(witness, trigger, name) {
    verifyUnsupportedReservation(witness, trigger.decline, name)
    verifyNativeEvents(witness, { execute: false })
    assert.ok(!witness.events.some(event => ['native_generation_capture_witness', 'native_generation_proof'].includes(event.command)),
        'A refused trigger row has no native preparation witness or proof')
}

async function runTriggerScenario({ name, driver, fixture, manifest, restart }) {
    const trigger = driver.trigger
    assert.ok(trigger?.scenario === name, 'Trigger startup qualified the real TS oracle')
    const oracleIdentity = { fixtureSha256: trigger.fixtureSha256, oracleSha256: trigger.oracleSha256, caseId: trigger.caseId }
    const observation = trigger.expected.triggerObservation
    if (name === 'TRIGGER-STOP') {
        fixture.setReply({ content: reply })
        const beforeRows = await driver.messages()
        await driver.begin()
        await driver.send()
        verifyUnsupportedReservation(await reservationOutcome(driver, 'trigger stop reservation outcome'), trigger.decline, name)
        const stored = await settledRead(() => driver.conversation(), value => value.message.length === beforeRows.length + 1
            && isDeepStrictEqual(value.scriptstate, observation.storedScriptstate), 'JS stopping trigger stored state')
        await driver.idle()
        const snapshot = await driver.profile(true)
        verifyTriggerStopControl(snapshot, manifest)
        // The profile proves the JS Send ended before any request; the fixture confirms no late request reached it.
        await delay(2_000)
        assert.equal(fixture.requestCount, 0, 'A stopping start trigger sends no provider request')
        assert.deepEqual(await driver.errorDialogs(), [], 'A stopping start trigger shows no error dialog')
        const witness = await driver.witness()
        verifyRefusedTriggerWitness(witness, trigger, name)
        const terminal = await driver.conversation()
        verifyTriggerState(terminal, trigger, 'terminal')
        verifyTriggerStopRows(terminal.message, beforeRows)
        await driver.shown(terminal.message.at(-1).chatId, input)
        return { name, passed: true, oracleIdentity, witness, snapshot, requests: fixture.requestCount,
            storedScriptstate: terminal.scriptstate, storedNote: terminal.note, beforeRows, rows: terminal.message, settledRows: stored.message.length }
    }
    fixture.setReply({ content: trigger.responseContent })
    if (name === 'TRIGGER-DECLINE') {
        await driver.begin()
        await driver.send(false, trigger.input)
        verifyUnsupportedReservation(await reservationOutcome(driver, 'trigger refusal reservation outcome'), trigger.decline, name)
        await fixture.waitForRequests(1)
        await driver.idle()
        const witness = await driver.witness()
        verifyRefusedTriggerWitness(witness, trigger, name)
        const snapshot = await driver.profile(true)
        verifyJsControl(snapshot, manifest)
        verifyJsStartTrigger(snapshot)
        verifyLoreRequest(fixture.requests, trigger, null, 'Trigger fallback')
        const stored = await settledRead(() => driver.conversation(), value => value.message.at(-1)?.data === trigger.expected.output.text
            && isDeepStrictEqual(value.scriptstate, observation.storedScriptstate), 'JS trigger fallback stored state')
        verifyTriggerState(stored, trigger, 'terminal')
        const output = verifyTriggerRows(stored.message, trigger, 'Trigger fallback')
        await driver.shown(output.chatId, output.data)
        assert.equal(fixture.requestCount, 1, 'The JS route sends once')
        return { name, passed: true, oracleIdentity, witness, snapshot, requests: fixture.requestCount,
            storedScriptstate: stored.scriptstate, storedNote: stored.note, rows: stored.message, output, request: fixture.requests[0].json }
    }
    fixture.hold()
    await driver.begin()
    await driver.send(false, trigger.input)
    const ticket = await driver.ticket()
    await fixture.waitForRequests(1)
    await until(() => driver.status(ticket), value => value.dispatch === 'issued', 'Trigger Issued')
    const issuedProof = await driver.invoke('native_generation_proof', { ticket, normalization: null })
    verifyIssued(issuedProof)
    await until(() => driver.witness(), hasStatusAfterIssuedProof, 'Trigger product Issued adoption')
    const issuedConversation = await driver.conversation()
    verifyTriggerState(issuedConversation, trigger, 'issued')
    verifyLoreRequest(fixture.requests, trigger, null, 'Trigger')
    assertNoJsProcessing(await driver.profile(), manifest)
    fixture.release()
    const status = await driver.settled(ticket)
    assert.equal(status.phase, 'completed')
    assert.equal(status.output, 'committed')
    await driver.idle()
    assert.deepEqual(await driver.errorDialogs(), [], 'Trigger native Send shows no error dialog')
    const witness = await driver.witness()
    verifyNativeEvents(witness)
    verifyTerminalWitness(witness, ticket, 'completed')
    const snapshot = await driver.profile(true)
    assertNoJsProcessing(snapshot, manifest)
    const proof = await driver.invoke('native_generation_proof', { ticket, normalization: null })
    verifyTriggerReceipts(proof, issuedProof, trigger.units)
    assert.ok(proof.receipts.output)
    assert.equal(proof.outputMessageId, status.messageId)
    assert.equal(proof.failedWithOutput, false)
    const terminal = await driver.conversation()
    verifyTriggerState(terminal, trigger, 'terminal')
    const output = verifyTriggerRows(terminal.message, trigger)
    assert.deepEqual(terminal.message.slice(0, -1), issuedConversation.message, 'Trigger output preserves all submitted rows')
    assert.equal(output.chatId, status.messageId)
    await driver.shown(status.messageId, trigger.expected.output.text)
    let restartedConversation
    const restartEvidence = await verifyRestart(await restart(driver), manifest, snapshot, async current => {
        restartedConversation = await current.conversation()
        assert.deepEqual(restartedConversation.message, terminal.message, 'Trigger ordered rows survive actual restart')
        verifyTriggerState(restartedConversation, trigger, 'restart')
        await current.shown(status.messageId, trigger.expected.output.text)
    })
    assert.equal(fixture.requestCount, 1, 'Trigger restart never redispatches')
    return { name, passed: true, oracleIdentity, ticket, status, issuedProof, proof, witness, snapshot,
        requests: fixture.requestCount, restartEvidence, restartVerified: true,
        storedMaps: { issued: issuedConversation.scriptstate, terminal: terminal.scriptstate, restart: restartedConversation.scriptstate },
        storedNotes: { issued: issuedConversation.note, terminal: terminal.note, restart: restartedConversation.note },
        request: fixture.requests[0].json }
}

export async function runScenario({ name, driver, fixture, manifest, restart, crash, crashProcess, nativeControl = null }) {
    assert.ok(scenarios.includes(name))
    if (nativeControl) assert.ok(nativeEntryScenarios.includes(name))
    else assert.ok(!Object.hasOwn(nativeGateCases, name), 'Native gates require the explicit test executable')
    if (name === 'LORE' || name === 'MODULE' || name === 'LORE-MODULE') return runLoreScenario({ name, driver, fixture, manifest, restart })
    if (Object.hasOwn(triggerScenarios, name)) return runTriggerScenario({ name, driver, fixture, manifest, restart })
    const nativeGate = nativeControl ? nativeGateCases[name] ?? 'none' : null
    fixture.setReply({ content: reply })
    const arrayCase = outputData.cases.find(value => value.id === 'array-coercion')
    assert.ok(arrayCase, 'Policy-A fixture must exist')
    if (name === 'PERR-HTTP') fixture.setReply({ status: 500, body: { error: 'Synthetic provider failure' } })
    if (name === 'PERR-A') fixture.setReply({ body: { message: { role: 'assistant', content: arrayCase.content }, done: true } })
    if (name === 'PERR-JSON') fixture.setReply({ body: '{"message":' })
    if (name === 'PERR-NUMBER') {
        const numberCase = outputData.cases.find(value => value.id === 'number-without-thinking')
        assert.ok(numberCase, 'Numeric transform fixture must exist')
        fixture.setReply({ body: { message: { role: 'assistant', content: numberCase.content }, done: true } })
    }
    if (name === 'CANA-BODY') fixture.holdBody(32)
    else if (!takesJsRoute(name) && !['UNSUPPORTED', 'CANB', 'SUCC-UNHELD'].includes(name)) fixture.hold()
    await driver.begin()
    if (name === 'CANB' || nativeControl) await driver.arm('before-execute')
    if (name === 'LOST-COMMIT') await driver.arm('terminal-proof')
    await driver.send(name === 'DUP-SEND')
    if (name === 'UNSUPPORTED') {
        verifyUnsupportedReservation(await until(() => driver.witness(), value => value.events.some(event =>
            event.command === 'native_generation_reserve_ollama' && event.state !== 'pending'), 'product reservation outcome'))
        // The refused reservation runs no native generation, and the Send continues on the JS route.
        await fixture.waitForRequests(1)
        await driver.idle()
        const witness = await driver.witness()
        verifyUnsupportedReservation(witness)
        verifyNativeEvents(witness, { execute: false })
        assert.ok(!witness.events.some(event => event.command === 'native_generation_capture_witness'), 'A refused reservation captures no witness')
        const snapshot = await driver.profile(true)
        verifyJsControl(snapshot, manifest)
        const stored = await until(() => driver.messages(), rows => rows.filter(value => value.data === reply).length === 1, 'JS persisted output')
        await driver.shown(stored.find(value => value.data === reply).chatId, reply)
        assert.equal(fixture.requestCount, 1)
        return { name, passed: true, witness, snapshot, requests: 1 }
    }
    if (takesJsRoute(name)) {
        await fixture.waitForRequests(1)
        await driver.idle()
        const snapshot = await driver.profile(true)
        verifyJsControl(snapshot, manifest, { historyLimit: hasHistoryLimit(name) })
        assert.equal((await driver.witness()).events.length, 0, name === 'NATIVE-HISTORY'
            ? 'A send whose history window applies makes no native reservation' : 'Opt-out uses no native generation commands')
        const stored = await until(() => driver.messages(), rows => rows.filter(value => value.data === reply).length === 1, 'JS persisted output')
        const output = stored.find(value => value.data === reply)
        await driver.shown(output.chatId, reply)
        assert.equal(fixture.requestCount, 1)
        let restartEvidence = null
        if (hasHistoryLimit(name)) {
            restartEvidence = await verifyJsRestart(await restart(driver), manifest, snapshot, stored, output)
            assert.equal(fixture.requestCount, 1, 'JS history restart never redispatches')
        }
        return { name, passed: true, snapshot, requests: 1, restartEvidence }
    }
    const ticket = await driver.ticket()
    if (nativeControl) {
        await driver.barrier('before-execute')
        await nativeControl.arm(driver, ticket, nativeGate)
        await driver.resume()
    }
    let nativeGateObservation = null
    let preAdmissionCancellation = null
    let issuedProof = null
    let beforeLoss = null
    let beforeWitness = null
    let beforeCommittedRows = null
    let bodyCancellation = null
    if (name === 'CANB') {
        await driver.barrier('before-execute')
        await driver.stop()
        await driver.resume()
        await until(() => driver.witness(), value => value.events.some(event =>
            event.command === 'native_generation_cancel' && event.state === 'resolved'
            && event.result?.phase === 'cancelled' && event.result.cancelDecision === null),
            'real product reserved cancellation')
    } else if (name === 'SUCC-UNHELD') {
        // The reply is never held, so the owner can commit output while the product still adopts its Issued proof.
        await fixture.waitForRequests(1)
    } else {
        await fixture.waitForRequests(1)
        await until(() => driver.status(ticket), status => status.dispatch === 'issued', 'Issued')
        issuedProof = await driver.invoke('native_generation_proof', { ticket, normalization: null })
        verifyIssued(issuedProof)
        const issuedWitness = await until(() => driver.witness(), hasStatusAfterIssuedProof,
            'real product Issued proof followed by status observation')
        verifyIssued(issuedWitness.events.find(event => event.command === 'native_generation_proof' && event.result?.at === 'issued').result)
        assert.ok(hasStatusAfterIssuedProof(issuedWitness),
            'Real product observes native status after Issued proof, possibly waiting for the held reply')
        assertNoJsProcessing(await driver.profile(), manifest)
        if (nativeControl) {
            fixture.release()
            if (nativeGate !== 'none') {
                const cancellation = await cancelAtNativeGate(nativeControl, driver, ticket, nativeGate)
                nativeGateObservation = cancellation.gateObservation
                preAdmissionCancellation = cancellation.preAdmissionCancellation
            }
        }
        if (name === 'DUP-RESTART') {
            const before = await driver.profile()
            const witness = await driver.witness()
            verifyNativeEvents(witness)
            const rows = await driver.messages()
            driver = await crashProcess()
            await driver.invoke('pds_open')
            const recovered = await driver.settled(ticket)
            assert.equal(recovered.phase, 'interrupted')
            assert.equal(recovered.output, 'none')
            assert.equal(recovered.cancelDecision, null)
            const recoveryProof = await driver.invoke('native_generation_proof', { ticket, normalization: null })
            assert.equal(recoveryProof.at, 'interrupted')
            assert.equal(recoveryProof.receipts.output, null)
            assert.deepEqual(await driver.messages(), rows)
            fixture.release()
            await driver.begin()
            await driver.startApp()
            await driver.shown(rows.at(-1).chatId, input)
            const after = await driver.profile(true)
            const combined = structuredClone(after)
            for (const [id, counts] of Object.entries(before.counts)) {
                for (const [engine, value] of Object.entries(counts)) combined.counts[id][engine] += value
            }
            assertNoJsProcessing(combined, manifest)
            const recoveryWitness = await driver.witness()
            assert.equal(executes(recoveryWitness).length, 0, 'Interrupted recovery never executes generation')
            assert.deepEqual(await driver.messages(), rows)
            assert.equal(fixture.requestCount, 1, 'Interrupted recovery never redispatches')
            return { name, passed: true, ticket, issuedProof, recoveryProof, status: recovered,
                snapshot: before, recoverySnapshot: after, witness, recoveryWitness, requests: 1, processCrashInjected: true }
        }
        if (name === 'CANA') await driver.stop()
        if (name === 'CANA-BODY') bodyCancellation = await cancelHeldBody(driver, fixture, ticket)
        if (name === 'NAV') {
            await driver.navigate('Synthetic other')
            await driver.shown('message-other', otherText)
            assert.equal((await driver.status(ticket)).phase, 'running', 'Navigation leaves original generation running')
        }
        if (name === 'EDIT') await driver.editEarlier()
        if (name === 'LOST-HOLD') {
            beforeLoss = await driver.profile()
            beforeWitness = await driver.witness()
            driver = await crash(driver)
            await driver.begin()
            const active = await driver.invoke('native_generation_active')
            assert.ok(active.some(value => value.ticket.epoch === ticket.epoch && value.ticket.sequence === ticket.sequence))
            await driver.startApp()
        }
        if (['CANA', 'EDIT'].includes(name)) await driver.settled(ticket)
        if (!nativeControl && name !== 'CANA-BODY') fixture.release()
    }
    if (name === 'LOST-COMMIT') {
        await driver.barrier('terminal-proof')
        const status = await driver.status(ticket)
        assert.equal(status.output, 'committed')
        beforeCommittedRows = await driver.messages()
        assert.equal(beforeCommittedRows.filter(value => value.chatId === status.messageId).length, 1)
        assert.equal(await driver.evaluate(`!!document.querySelector(${q(row(status.messageId))})`), false,
            'Committed output has not reached the real UI')
        beforeLoss = await driver.profile()
        beforeWitness = await driver.witness()
        driver = await crash(driver)
        await driver.begin()
        await driver.startApp()
    }
    const status = await driver.settled(ticket)
    if (name === 'SUCC-UNHELD') verifyUnheldCompletion(await driver.witness(), status)
    assert.equal(status.phase, ['CANB', 'CANA', 'CANA-BODY', 'EDIT', ...Object.keys(nativeGateCases)].includes(name) ? 'cancelled'
        : name.startsWith('PERR') ? 'failed' : 'completed')
    if (name === 'CANB') assert.equal(status.cancelDecision, null)
    if (['CANA', 'CANA-BODY', ...Object.keys(nativeGateCases)].includes(name)) assert.equal(status.cancelDecision, 'user')
    if (name === 'EDIT') assert.equal(status.cancelDecision, 'edit')
    if (name === 'PERR-HTTP') await driver.error('Synthetic provider failure')
    if (name === 'PERR-A') await driver.error('trim')
    if (['PERR-JSON', 'PERR-NUMBER'].includes(name)) await driver.error(verifyWireFailure(name, status))
    await driver.idle()
    if (name === 'SUCC-UNHELD') assert.deepEqual(await driver.errorDialogs(), [], 'An unheld native completion shows no error dialog')
    const witness = await driver.witness()
    let recoveryKind = null
    const recoveredCommittedRows = beforeCommittedRows ? await driver.messages() : null
    if (beforeWitness) {
        recoveryKind = verifyRecoveryWitness(beforeWitness, witness, ticket, status.phase, {
            committed: name === 'LOST-COMMIT', beforeRows: beforeCommittedRows, recoveredRows: recoveredCommittedRows,
        })
    } else {
        verifyNativeEvents(witness, { execute: name !== 'CANB' })
        verifyTerminalWitness(witness, ticket, status.phase, { requireStatus: name !== 'CANB' })
    }
    const snapshot = await driver.profile(true)
    if (beforeLoss) {
        assertNoJsProcessing(beforeLoss, manifest)
        const combined = structuredClone(snapshot)
        for (const [id, counts] of Object.entries(beforeLoss.counts)) {
            for (const [engine, value] of Object.entries(counts)) combined.counts[id][engine] += value
        }
        assertNoJsProcessing(combined, manifest)
    } else assertNoJsProcessing(snapshot, manifest)
    const proof = await driver.invoke('native_generation_proof', { ticket, normalization: null })
    if (wireCases.includes(name)) verifyWireProof(name, status, proof, issuedProof)
    if (nativeControl) verifyNativeGateOutcome(name, ticket, status, proof, issuedProof)
    if (name === 'CANB') {
        assert.equal(status.dispatch, 'none')
        assert.equal(status.checkpoint, '0')
        assert.deepEqual(proof.receipts.effects, [])
        assert.equal(proof.receipts.output, null)
        assert.equal(proof.outputMessageId, null)
    }
    const rows = await driver.messages()
    const expectedOutput = name === 'PERR-A' ? arrayCase.output : reply
    const expectedDisplayedOutput = name === 'PERR-A' ? arrayCoercionDisplayedOutput : expectedOutput
    const hasOutput = status.phase === 'completed' || name === 'PERR-A' || name === 'CAN-ADMITTED'
    assert.equal(status.output, hasOutput ? 'committed' : 'none')
    assert.equal(rows.length, hasOutput ? 4 : 3)
    assert.equal(rows.filter(value => value.data === input).length, 1, 'One DOM submission')
    assert.equal(rows.filter(value => value.chatId === status.messageId).length, hasOutput ? 1 : 0)
    if (hasOutput) {
        assert.ok(proof.receipts.output)
        assert.equal(proof.outputMessageId, status.messageId)
        assert.equal(rows.find(value => value.chatId === status.messageId).data, expectedOutput)
        if (name !== 'NAV') await driver.shown(status.messageId, expectedDisplayedOutput)
    }
    assert.equal(proof.failedWithOutput, name === 'PERR-A')
    if (name === 'NAV') {
        await driver.shown('message-other', otherText)
        assert.equal(await driver.evaluate(`!!document.querySelector(${q(row(status.messageId))})`), false)
        assert.deepEqual(await driver.messages('conversation-other'),
            [{ role: 'user', data: otherText, chatId: 'message-other' }])
    }
    if (name === 'EDIT') {
        assert.equal(rows.find(value => value.chatId === 'message-a').data, edited)
        await driver.shown('message-a', edited)
    }
    assert.equal(fixture.requestCount, name === 'CANB' ? 0 : 1)
    const otherRows = name === 'NAV' ? await driver.messages('conversation-other') : null
    const nativeTrace = nativeControl ? await nativeControl.finalize(name) : null
    if (nativeControl) nativeControl.requireFinalizedBeforeRestart()
    const restarted = await restart(driver)
    let restartedRows = null
    const restartEvidence = await verifyRestart(restarted, manifest, beforeLoss ?? snapshot, async current => {
        restartedRows = await current.messages()
        assert.deepEqual(restartedRows, rows, 'Full ordered rows survive actual app restart')
        if (name === 'NAV') {
            assert.deepEqual(await current.messages('conversation-other'), otherRows)
            await current.shown('message-other', otherText)
            await current.navigate('Synthetic conversation')
        }
        if (hasOutput) await current.shown(status.messageId, expectedDisplayedOutput)
        if (name === 'EDIT') await current.shown('message-a', edited)
    })
    assert.equal(fixture.requestCount, name === 'CANB' ? 0 : 1, 'Restart never redispatches')
    const staleNativeToken = nativeControl ? await verifyStaleNativeToken(restarted, nativeTrace.token) : null
    return { name, passed: true, requests: fixture.requestCount, ticket, status, issuedProof, proof,
        snapshot, beforeLoss, witness, beforeWitness, recoveryKind, bodyCancellation,
        committedSnapshot: beforeCommittedRows ? { beforeRows: beforeCommittedRows, recoveredRows: recoveredCommittedRows } : null,
        restartEvidence, restartVerified: true, nativeGateObservation, preAdmissionCancellation, nativeTrace, staleNativeToken,
        nativeGateRows: nativeControl ? { beforeRestart: rows, afterRestart: restartedRows } : null }
}

