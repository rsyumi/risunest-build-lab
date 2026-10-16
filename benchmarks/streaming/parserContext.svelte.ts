import rfdc from 'rfdc'
import { createChatParserContextSeed, forkChatParserContext } from '../../src/ts/chatParserContextSeed.svelte'
import type { ProcessScriptCaptureContext } from '../../src/ts/process/scripts'

const clone = rfdc()

export function createParserContextFixture(assets: number): ProcessScriptCaptureContext {
    const character = {
        type: 'character', chaId: 'synthetic', name: 'Synthetic', chatPage: 0,
        chats: [{ id: 'synthetic-chat', message: [], scriptstate: {} }],
        additionalAssets: Array.from({ length: assets }, (_, index) => [`asset-${index}`, `synthetic-${index}.png`, 'png']),
    }
    const context = $state({
        presetRegex: [], moduleRegexScripts: [], moduleAssets: [], dynamicAssets: false, dynamicAssetsEditDisplay: false,
        parserContext: { database: { characters: [character] }, character, modules: [], moduleLorebooks: [],
            userName: 'Synthetic user', personaPrompt: '', selectedCharID: 0, chatVariables: {}, globalChatVariables: {}, currentTime: 1 },
    } as unknown as ProcessScriptCaptureContext)
    return context
}

export function prepareParserRows(context: ProcessScriptCaptureContext, optimized: boolean, rows = 48): ProcessScriptCaptureContext[] {
    const seed = optimized ? createChatParserContextSeed(context) : context
    return Array.from({ length: rows }, (_, index) => {
        const row = optimized ? forkChatParserContext(seed) : clone(seed)
        const parser = row.parserContext
        parser.character.chats[0].message = [{ role: 'char', data: `row-${index}` }]
        parser.database.characters[0] = parser.character
        parser.chatVariables.row = String(index)
        return row
    })
}

export function measureParserContext(assets: number, rows = 48) {
    const context = createParserContextFixture(assets)
    const measure = (optimized: boolean) => {
        const start = performance.now()
        const prepared = prepareParserRows(context, optimized, rows)
        if (prepared.at(-1)!.parserContext.chatVariables.row !== String(rows - 1)
            || prepared[0].parserContext.chatVariables.row !== '0'
            || context.parserContext.character.chats[0].message.length !== 0) throw new Error('parser-context-isolation-failed')
        return performance.now() - start
    }
    return { assets, rows, proxied: true, beforeMs: measure(false), afterMs: measure(true) }
}
