import { expect, it, vi } from 'vitest'
import type { OpenAIChat } from '../../src/ts/process/index.svelte'
import type { Database } from '../../src/ts/storage/database.svelte'
import { applyChatTemplate, chatTemplates, templateEffect } from '../../src/ts/process/templates/chatTemplate'
import { templateCheck } from '../../src/ts/process/templates/templateCheck'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

type Settings = { type?: string; custom?: string; instructChatTemplate?: string; JinjaTemplate?: string; charName?: string; userName: string }
type GlueCase = OracleCase & (
    | { kind: 'builtins' }
    | { kind: 'chat'; messages: { role: string; content: string }[]; settings: Settings; decline?: string }
    | { kind: 'check'; promptTemplate?: unknown; decline?: string }
)

// What the mocked database and character answer for the running case.
const state: { db: Partial<Database>; character: { name?: string }; userName: string } = { db: {}, character: {}, userName: '' }

vi.mock('src/ts/storage/database.svelte', () => ({
    getDatabase: () => state.db as Database,
    getCurrentCharacter: () => state.character as ReturnType<typeof import('src/ts/storage/database.svelte').getCurrentCharacter>,
}) satisfies Pick<typeof import('src/ts/storage/database.svelte'), 'getDatabase' | 'getCurrentCharacter'>)
vi.mock('src/ts/util', () => ({ getUserName: () => state.userName }) satisfies Pick<typeof import('src/ts/util'), 'getUserName'>)

// The case file comes from templates-glue.cases.mjs and drives the real template functions.
const settle = {
    generator: import.meta.url,
    tsSources: ['src/ts/process/templates/chatTemplate.ts', 'src/ts/process/templates/templateCheck.ts'],
}
const set = readOracleCases<GlueCase>('templates', 'glue')
const message = (error: unknown) => error instanceof Error ? error.message : String(error)

function run(entry: GlueCase): unknown {
    switch (entry.kind) {
        case 'builtins':
            return { templates: Object.entries(chatTemplates), effects: Object.entries(templateEffect) }
        case 'chat': {
            const { type, custom, instructChatTemplate, JinjaTemplate, charName, userName } = entry.settings
            state.db = {
                ...(instructChatTemplate === undefined ? {} : { instructChatTemplate }),
                ...(JinjaTemplate === undefined ? {} : { JinjaTemplate }),
            }
            state.character = charName === undefined ? {} : { name: charName }
            state.userName = userName
            const arg = { ...(type === undefined ? {} : { type }), ...(custom === undefined ? {} : { custom }) }
            const messages = structuredClone(entry.messages) as OpenAIChat[]
            try {
                const ok = applyChatTemplate(messages, arg)
                expect(messages).toEqual(entry.messages)
                return { ok }
            } catch (error) {
                return { error: message(error) }
            }
        }
        case 'check': {
            const db = ('promptTemplate' in entry ? { promptTemplate: entry.promptTemplate } : {}) as Database
            try {
                return { ok: templateCheck(db) }
            } catch (error) {
                return { error: message(error) }
            }
        }
    }
}

it('renders the template glue cases', () => {
    const results = set.cases.map(run)
    expect(results.some(result => typeof result === 'object' && result !== null && 'ok' in result)).toBe(true)
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
