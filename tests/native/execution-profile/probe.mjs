import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'
import { installExecutionProfile } from './profile.mjs'

export const repositoryRoot = fileURLToPath(new URL('../../../', import.meta.url))
export const defaultLedger = JSON.parse(readFileSync(new URL('./js-processing-ledger.json', import.meta.url), 'utf8'))
const digest = text => createHash('sha256').update(text).digest('hex')
const classes = new Set(['must-be-native', 'allowed-webview', 'web-only'])
const instrumentation = Object.fromEntries(['probe.mjs', 'profile.mjs', 'vite.config.ts', 'js-processing-ledger.json']
    .map(file => ['tests/native/execution-profile/' + file, digest(readFileSync(new URL(file, import.meta.url), 'utf8'))]))

export function createManifest(ledger, sources) {
    if (ledger?.schema !== 'risunest.execution-profile/v1' || !ledger.scenario || !ledger.entries?.length) {
        throw new Error('execution-profile:invalid-ledger')
    }
    const ids = new Set()
    const targets = new Set()
    for (const entry of ledger.entries) {
        const target = JSON.stringify([entry.module, entry.kind, entry.owner, entry.symbol])
        if (!entry.id || ids.has(entry.id) || targets.has(target) || !classes.has(entry.class)
            || !/^src\/(?!.*(?:\.\.|\\)).+\.ts$/.test(entry.module)
            || !['function', 'method'].includes(entry.kind) || !/^[A-Za-z_$][\w$]*$/.test(entry.symbol)
            || (entry.kind === 'method' ? !entry.owner : entry.owner !== undefined)
            || (entry.modeParameter !== undefined && (!/^[A-Za-z_$][\w$]*$/.test(entry.modeParameter)
                || !Array.isArray(entry.allowedModes) || entry.allowedModes.some(mode => typeof mode !== 'string')))
            || typeof sources[entry.module] !== 'string') {
            throw new Error(`execution-profile:invalid-entry:${entry.id}`)
        }
        ids.add(entry.id)
        targets.add(target)
    }
    const fingerprints = Object.fromEntries([...new Set(ledger.entries.map(entry => entry.module))].sort()
        .map(module => [module, digest(sources[module])]))
    const lineage = { schema: ledger.schema, scenario: ledger.scenario,
        ledgerSha256: digest(JSON.stringify(ledger)), sources: fingerprints, instrumentation }
    return JSON.parse(JSON.stringify({ ...lineage, identity: digest(JSON.stringify(lineage)), entries: ledger.entries }))
}

export function instrumentSource(code, module, manifest) {
    const entries = manifest.entries.filter(entry => entry.module === module)
    if (!entries.length) return null
    if (digest(code) !== manifest.sources[module]) throw new Error(`execution-profile:source-changed:${module}`)
    const source = ts.createSourceFile(module, code, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
    const insertions = []
    for (const entry of entries) {
        const matches = []
        const visit = node => {
            if (node.body && node.name?.getText(source) === entry.symbol) {
                const matched = entry.kind === 'function'
                    ? ts.isFunctionDeclaration(node) && ts.isSourceFile(node.parent)
                    : ts.isMethodDeclaration(node) && ts.isClassDeclaration(node.parent) && node.parent.name?.text === entry.owner
                if (matched) matches.push(node)
            }
            ts.forEachChild(node, visit)
        }
        visit(source)
        if (matches.length !== 1) throw new Error(`execution-profile:target-count:${entry.id}:${matches.length}`)
        const node = matches[0]
        if (entry.modeParameter && !node.parameters.some(parameter => ts.isIdentifier(parameter.name) && parameter.name.text === entry.modeParameter)) {
            throw new Error(`execution-profile:mode-parameter:${entry.id}`)
        }
        let at = node.body.getStart(source) + 1
        for (const statement of node.body.statements) {
            if (!ts.isExpressionStatement(statement) || !ts.isStringLiteral(statement.expression)) break
            at = statement.end
        }
        insertions.push({ at, text: `\n;(globalThis as any).__risunestExecutionProfile.enter(${JSON.stringify(manifest.identity)}, ${JSON.stringify(entry.id)}, ${entry.modeParameter ?? 'undefined'});\n` })
    }
    for (const insertion of insertions.sort((a, b) => b.at - a.at)) {
        code = code.slice(0, insertion.at) + insertion.text + code.slice(insertion.at)
    }
    return { code, map: null }
}

export function executionProfileProbe({ root = repositoryRoot, ledger = defaultLedger } = {}) {
    root = resolve(root)
    const modules = [...new Set(ledger.entries.map(entry => entry.module))]
    const sources = Object.fromEntries(modules.map(module => [module, readFileSync(resolve(root, module), 'utf8')]))
    const manifest = createManifest(ledger, sources)
    // Validate every declared target even if its module later disappears from the build graph.
    for (const module of modules) instrumentSource(sources[module], module, manifest)
    const visited = new Set()
    return {
        name: 'risunest-execution-profile',
        apply: 'build',
        enforce: 'pre',
        buildStart() { visited.clear() },
        transform(code, id) {
            const module = relative(root, id.split('?')[0]).replaceAll('\\', '/')
            if (!Object.hasOwn(manifest.sources, module)) return null
            const result = instrumentSource(code, module, manifest)
            visited.add(module)
            return result
        },
        transformIndexHtml: { order: 'pre', handler() {
            return [{ tag: 'script', injectTo: 'head-prepend',
                children: `(${installExecutionProfile.toString()})(${JSON.stringify(manifest)});` }]
        } },
        generateBundle() {
            for (const module of modules) {
                if (!visited.has(module)) throw new Error(`execution-profile:module-not-instrumented:${module}`)
                if (digest(readFileSync(resolve(root, module), 'utf8')) !== manifest.sources[module]) {
                    throw new Error(`execution-profile:source-changed:${module}`)
                }
            }
            for (const [file, expected] of Object.entries(manifest.instrumentation)) {
                if (digest(readFileSync(resolve(root, file), 'utf8')) !== expected) throw new Error('execution-profile:instrumentation-changed')
            }
            this.emitFile({ type: 'asset', fileName: 'execution-profile-manifest.json', source: JSON.stringify(manifest, null, 2) })
        },
    }
}
