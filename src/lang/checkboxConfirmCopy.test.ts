import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { externalStorageStrings } from '../lib/Setting/ExternalStorage/strings'
import { languageEnglish } from './en'
import { languageKorean } from './ko'

function asksAgain(text: string): boolean {
    return text.includes('?') || ['No를', '정말', 'Really'].some((question) => text.includes(question))
}

function sourceFiles(directory: string): string[] {
    return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
        const path = join(directory, entry.name).replaceAll('\\', '/')
        if (entry.isDirectory()) return sourceFiles(path)
        return /\.(ts|svelte)$/.test(entry.name) && !/\.test\.|\.d\.ts$/.test(entry.name) ? [path] : []
    })
}

function resolvePath(root: unknown, path: string[]): unknown {
    let value = root
    for (const key of path) value = value && typeof value === 'object' ? (value as Record<string, unknown>)[key] : undefined
    // A composed message is checked in the form its call site uses, without the trailing question.
    return typeof value === 'function' ? value('Synthetic', 1, 1, false) : value
}

/** The texts each `alertCheckboxConfirm` description can show, by call site. */
function checkboxDescriptions() {
    const sites: { site: string; texts: string[] }[] = []
    for (const file of sourceFiles('src')) {
        const source = readFileSync(file, 'utf8')
        const aliases = new Map<string, (locale: 'ko' | 'en') => unknown>([
            ['language', (locale) => locale === 'ko' ? languageKorean : languageEnglish],
        ])
        for (const match of source.matchAll(/const (\w+) = language((?:\.\w+)+)/g)) {
            const path = match[2].slice(1).split('.')
            aliases.set(match[1], (locale) => resolvePath(locale === 'ko' ? languageKorean : languageEnglish, path))
        }
        for (const match of source.matchAll(/const (\w+) = \$derived\(externalStorageStrings\(/g)) {
            aliases.set(match[1], (locale) => externalStorageStrings(locale))
        }
        for (const call of source.matchAll(/alertCheckboxConfirm\(\{/g)) {
            const start = call.index!
            const end = source.indexOf('checkboxLabel', start)
            const segment = source.slice(source.indexOf('description', start) + 'description'.length, end)
            let expression = segment.trimStart().startsWith(':') ? segment : 'description'
            for (const name of new Set(expression.match(/\b[A-Za-z_]\w*\b/g) ?? [])) {
                if (aliases.has(name)) continue
                const declaration = source.lastIndexOf(`const ${name} = `, start)
                if (declaration >= 0) expression += source.slice(declaration, start)
            }
            const texts: string[] = []
            for (const reference of expression.matchAll(/\b(\w+)((?:\.\w+)+)/g)) {
                const root = aliases.get(reference[1])
                if (!root) continue
                for (const locale of ['ko', 'en'] as const) {
                    const text = resolvePath(root(locale), reference[2].slice(1).split('.'))
                    if (typeof text === 'string') texts.push(text)
                }
            }
            sites.push({ site: `${file}:${source.slice(0, start).split('\n').length}`, texts })
        }
    }
    return sites
}

describe('checkbox confirmation copy', () => {
    it('gives every checkbox dialog a statement for its description', () => {
        const sites = checkboxDescriptions()
        expect(sites.length).toBeGreaterThanOrEqual(15)
        const violations = sites.flatMap(({ site, texts }) => texts.length === 0
            ? [`${site}: no description found`]
            : texts.filter(asksAgain).map((text) => `${site}: ${text}`))
        expect(violations).toEqual([])
    })
})
