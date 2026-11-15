import { readFileSync, realpathSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import markdownit, { type MarkdownIt, type Options, type Ruler } from 'markdown-it'
import { expect, it } from 'vitest'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'
import { renderTables, tablesPath } from './render-markdown.tables.mjs'

// Strings that are not well-formed travel as their UTF-16 units, since serde_json rejects isolated surrogate escapes.
type Text = string | { u: number[] }
type MarkdownCase = OracleCase & { source: Text; decline?: string }
// `md` is the plain instance, `highlight` the `mdHighlight` one.
type Result = { md: Text; highlight: Text }

// The cases come from render-markdown.cases.mjs and run only the markdown-it package the app imports, built from the
// configuration text in the parser source, so no product module is executed or hashed. The versions are pinned below.
const settle = { generator: import.meta.url, tsSources: [] }
const set = readOracleCases<MarkdownCase>('render', 'markdown')
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const parserSource = readFileSync(join(repositoryRoot, 'src/ts/parser/parser.svelte.ts'), 'utf8').replaceAll('\r\n', '\n')

function decodeText(text: Text) {
    if (typeof text === 'string') return text
    let value = ''
    for (const unit of text.u) value += String.fromCharCode(unit)
    return value
}
function wellFormed(text: string) {
    for (let index = 0; index < text.length; index++) {
        const unit = text.charCodeAt(index)
        if (unit >= 0xdc00 && unit <= 0xdfff) return false
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = text.charCodeAt(index + 1)
            if (!(next >= 0xdc00 && next <= 0xdfff)) return false
            index++
        }
    }
    return true
}
const encodeText = (text: string): Text => wellFormed(text) ? text : { u: Array.from({ length: text.length }, (_, index) => text.charCodeAt(index)) }
// The rule list every Ruler keeps; the package types leave it out.
type RuleList = { __rules__: { name: string; enabled: boolean; fn: unknown }[] }
const rules = (ruler: Ruler<unknown>) => (ruler as unknown as RuleList).__rules__
const enabledRules = (ruler: Ruler<unknown>) => rules(ruler).filter(rule => rule.enabled).map(rule => rule.name)

function section(pattern: RegExp) {
    const match = parserSource.match(pattern)
    expect(match, `parser.svelte.ts no longer matches ${pattern}`).not.toBeNull()
    return match![1]
}

// What would make the app render differently from the two instances built below: a plugin, a renderer rule, an option
// set, or a rule added, enabled or disabled on either instance, or another markdown-it instance.
const driftPatterns = [
    /\b(?:md|mdHighlight)\s*\.\s*use\s*\(/,
    /\brenderer\s*\.\s*rules\b/,
    /\b(?:md|mdHighlight)\s*\.\s*(?:set|enable)\s*\(/,
    /\b(?:md|mdHighlight)\s*\.\s*(?:core|block|inline)\s*\.\s*ruler2?\b/,
]
function configurationDrift(source: string) {
    const problems = driftPatterns.filter(pattern => pattern.test(source)).map(pattern => `matches ${pattern}`)
    const instances = source.match(/\bmarkdownit\(/g)?.length ?? 0
    if (instances !== 2) problems.push(`markdownit( appears ${instances} times`)
    const disables = source.match(/\.\s*disable\s*\(/g)?.length ?? 0
    if (disables !== 2) problems.push(`disable( appears ${disables} times`)
    return problems
}

// The app's two instances, evaluated from their source text so the oracle cannot drift from the configuration.
function appInstances() {
    expect(configurationDrift(parserSource)).toEqual([])
    const optionsText = section(/\nconst markdownItOptions = (\{\n[\s\S]*?\n\})\n/)
    const highlightText = section(/\nconst mdHighlight = (markdownit\(\{\n[\s\S]*?\n\}\))\n/)
    expect(parserSource).toContain('\nconst md = markdownit(markdownItOptions)\n')
    expect(parserSource).toContain("\nmd.disable(['code'])\nmdHighlight.disable(['code'])\n")
    const markdownItOptions = new Function(`return ${optionsText}`)() as Options
    const md = markdownit(markdownItOptions)
    const mdHighlight = new Function('markdownit', 'markdownItOptions', `return ${highlightText}`)(markdownit, markdownItOptions) as MarkdownIt
    md.disable(['code'])
    mdHighlight.disable(['code'])
    return { md, mdHighlight }
}

it('notices a plugin, a renderer rule or other configuration added to the parser source', () => {
    expect(configurationDrift(parserSource)).toEqual([])
    const additions = ['md.use(plugin)', 'mdHighlight.use(plugin, {})', 'md.renderer.rules.fence = rule', 'mdHighlight.renderer.rules.text = rule',
        "mdHighlight.set({ breaks: false })", "md.enable(['code'])", 'const third = markdownit()', "md.core.ruler.push('rule', rule)",
        "mdHighlight.inline.ruler2.before('emphasis', 'rule', rule)", "md.disable(['table'])"]
    for (const addition of additions) expect(configurationDrift(`${parserSource}\n${addition}\n`), addition).not.toEqual([])
    expect(configurationDrift(parserSource.replace('const md = markdownit(markdownItOptions)', 'const md = mdHighlight'))).not.toEqual([])
})

// `normalizeReference` of each code point on its own; a label folds the same way one code point at a time.
it('records the case mapping of normalizeReference for every code point', () => {
    const normalize = (markdownit().utils as unknown as { normalizeReference: (label: string) => string }).normalizeReference
    const changed: [number, string][] = []
    for (let code = 0; code <= 0x10ffff; code++) {
        if (code >= 0xd800 && code <= 0xdfff) continue
        const character = String.fromCodePoint(code)
        const folded = normalize(character)
        if (folded !== character) changed.push([code, folded])
    }
    expect(changed.length).toBeGreaterThan(1000)
    expect(settleOracle(readOracleCases('render', 'reference-case'), [changed], settle)).toMatch(/written|matched/)
})

it('pins markdown-it and the dependencies the Rust renderer ports', () => {
    const markdownIt = realpathSync(dirname(createRequire(import.meta.url).resolve('markdown-it/package.json')))
    const version = (directory: string) => (JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8')) as { version: string }).version
    expect(version(markdownIt)).toBe('14.3.2')
    expect(Object.fromEntries(['entities', 'mdurl', 'punycode.js', 'uc.micro'].map(name => [name, version(join(markdownIt, '..', name))])))
        .toEqual({ entities: '4.5.0', mdurl: '2.1.0', 'punycode.js': '2.3.1', 'uc.micro': '2.1.0' })
})

it('keeps the generated decode and punctuation tables in step with the packages', async () => {
    expect(readFileSync(tablesPath, 'utf8').replaceAll('\r\n', '\n')).toBe(await renderTables())
})

it('builds the app configuration the Rust renderer ports', () => {
    const { md, mdHighlight } = appInstances()
    for (const instance of [md, mdHighlight]) {
        expect(instance.options).toMatchObject({ html: true, xhtmlOut: false, breaks: true, langPrefix: 'language-', linkify: false, typographer: true, quotes: '', maxNesting: 100 })
        expect(instance.block.ruler.getRules('')).not.toContain(rules(instance.block.ruler).find(rule => rule.name === 'code')!.fn)
        expect([instance.core.ruler, instance.block.ruler, instance.inline.ruler].map(enabledRules))
            .toEqual([
                ['normalize', 'block', 'inline', 'linkify', 'replacements', 'smartquotes', 'text_join'],
                ['table', 'fence', 'blockquote', 'hr', 'list', 'reference', 'html_block', 'heading', 'lheading', 'paragraph'],
                ['text', 'linkify', 'newline', 'escape', 'backticks', 'strikethrough', 'emphasis', 'link', 'image', 'autolink', 'html_inline', 'entity'],
            ])
        expect(enabledRules(instance.inline.ruler2)).toEqual(['balance_pairs', 'strikethrough', 'emphasis', 'fragments_join'])
    }
    expect(md.options.highlight).toBeNull()
    expect(mdHighlight.options.highlight!('a<b', 'js', '')).toBe('<pre-hljs-placeholder lang="js">a<b</pre-hljs-placeholder>')
    expect(mdHighlight.options.highlight!('a<b', '', '')).toBe('')
})

it('renders the markdown cases', () => {
    const { md, mdHighlight } = appInstances()
    const results = set.cases.map((entry): Result => {
        const source = decodeText(entry.source)
        return { md: encodeText(md.render(source)), highlight: encodeText(mdHighlight.render(source)) }
    })
    expect(results.some(result => result.md !== result.highlight)).toBe(true)
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
