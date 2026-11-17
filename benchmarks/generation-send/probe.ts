import ts from 'typescript'
import type { Plugin } from 'vite'

// Only the dedicated benchmark config installs these timing spans.
export function generationSendProbe(): Plugin {
    const fine = process.env.GENERATION_SEND_PROBES !== 'coarse'
    const functions: Record<string, Record<string, string>> = {
        '/src/ts/process/index.svelte.ts': {
            prepareGenerationHistoryWindow: 'history-window',
            runCurrentChatFunction: 'current-chat-parser',
            runChatOutputListeners: 'output-listeners',
        },
        '/src/ts/tokenizer.ts': { encode: 'tokenizer' },
    }
    if (fine) {
        functions['/src/ts/process/index.svelte.ts'].beginPromptHistoryOperation = 'prompt-history-operation'
        Object.assign(functions, {
            '/src/ts/process/request/request.ts': { requestChatDataUnderHold: 'provider-request-wrapper' },
            '/src/ts/process/triggers.ts': { runTriggerImpl: 'trigger-work' },
            '/src/ts/process/promptHistory.ts': { selectPromptHistory: 'prompt-history-selection' },
            '/src/ts/storage/activeConversationSession.ts': {
                scanBackward: 'session-backward-scan',
                scanMetadataBackward: 'session-metadata-backward-scan',
                readMetadataRange: 'session-metadata-range',
            },
        })
    }
    return {
        name: 'generation-send-probe',
        enforce: 'pre',
        transform(code, id) {
            const normalized = id.replaceAll('\\', '/').split('?')[0]
            const targets = Object.entries(functions).find(([suffix]) => normalized.endsWith(suffix))?.[1]
            if (!targets) return
            const source = ts.createSourceFile(id, code, ts.ScriptTarget.Latest, true)
            const insertions: { at: number, text: string }[] = []
            const found = new Set<string>()
            const detailed = fine && normalized.endsWith('/src/ts/process/index.svelte.ts')
            let serial = 0
            let templateLoops = 0
            const instrumentStatement = (node: ts.Node, label: string, zone = false) => {
                const index = serial++
                const previous = `__sendPreviousZone${index}`
                const start = `__sendStatementStart${index}`
                insertions.push({ at: node.getStart(source), text: `{ const ${start} = performance.now();`
                    + (zone ? ` const ${previous} = (globalThis as any).__generationSendZone; (globalThis as any).__generationSendZone = '${label}';` : '')
                    + ' try {\n' })
                insertions.push({ at: node.end, text: `\n} finally { (globalThis as any).__generationSendProbe?.('${label}', ${start}, performance.now());`
                    + (zone ? ` (globalThis as any).__generationSendZone = ${previous};` : '') + ' } }' })
            }
            const range = (startAnchor: string, endAnchor: string, label: string) => {
                const startAt = code.indexOf(startAnchor)
                const endAt = code.indexOf(endAnchor, startAt + startAnchor.length)
                if (startAt < 0 || endAt < 0 || code.indexOf(startAnchor, startAt + 1) >= 0) {
                    throw new Error(`Generation send range changed: ${label}`)
                }
                const local = `__sendRangeStart${serial++}`
                insertions.push({ at: startAt, text: `const ${local} = performance.now();\n` })
                insertions.push({ at: endAt, text: `;(globalThis as any).__generationSendProbe?.('${label}', ${local}, performance.now());\n` })
            }
            const visit = (node: ts.Node) => {
                if ((ts.isFunctionDeclaration(node) || ts.isMethodDeclaration(node)) && node.name && node.body) {
                    const name = node.name.getText(source)
                    if (targets[name]) {
                        found.add(name)
                        insertions.push({ at: node.body.getStart(source) + 1,
                            text: `\nconst __sendProbeStart = performance.now(); try {\n` })
                        insertions.push({ at: node.body.end - 1,
                            text: `\n} finally { (globalThis as any).__generationSendProbe?.('${targets[name]}', __sendProbeStart, performance.now()${fine && name === 'encode' ? ', data.length' : ''}); }\n` })
                    }
                }
                if (detailed && ts.isForOfStatement(node)) {
                    const expression = node.expression.getText(source)
                    if (expression === 'promptHistoryEntries') instrumentStatement(node, 'history-message-loop', true)
                    if (expression === 'formated') instrumentStatement(node, 'final-token-recheck', true)
                    if (expression === 'template') instrumentStatement(node,
                        templateLoops++ === 0 ? 'template-initial-materialization' : 'template-final-materialization', true)
                }
                if (detailed && ts.isWhileStatement(node)
                    && node.expression.getText(source).replaceAll(/\s/g, '') === 'currentTokens>maxContextTokens') {
                    instrumentStatement(node, 'context-trimming', true)
                }
                if (detailed && ts.isExpressionStatement(node)
                    && node.expression.getText(source).startsWith('console.log(JSON.stringify(chats,')) {
                    instrumentStatement(node, 'pretrim-prompt-log-serialization', true)
                }
                ts.forEachChild(node, visit)
            }
            visit(source)
            if (detailed) {
                range('    let unformated = {', '    const examples = exampleMessage', 'base-prompt-preparation')
                range('    const loreHistory = beginPromptHistoryOperation', '    const positionRegex =', 'lorebook-preparation')
                range('    const promptHistory = beginPromptHistoryOperation',
                    code.includes('    console.log(JSON.stringify(chats,')
                        ? '    console.log(JSON.stringify(chats,' : '    const depthPrompts =',
                    'prompt-history-preparation')
                range('    let formated:OpenAIChat[] = []', '    //token rechecking', 'final-prompt-assembly')
                if (templateLoops !== 2) throw new Error('Expected both initial and final template loops')
            }
            for (const name of Object.keys(targets)) {
                if (!found.has(name)) throw new Error(`Generation send probe target disappeared: ${name}`)
            }
            for (const insertion of insertions.sort((a, b) => b.at - a.at)) {
                code = code.slice(0, insertion.at) + insertion.text + code.slice(insertion.at)
            }
            return { code, map: null }
        },
    }
}
