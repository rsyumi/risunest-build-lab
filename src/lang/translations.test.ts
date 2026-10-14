import { expect, it } from 'vitest'
import { languageChinese } from './cn'
import { languageGerman } from './de'
import { languageEnglish } from './en'
import { languageSpanish } from './es'
import { languageKorean } from './ko'
import { languageVietnamese } from './vi'
import { languageChineseTraditional } from './zh-Hant'

const otherLanguages = [languageChinese, languageChineseTraditional, languageGerman, languageSpanish, languageVietnamese]

function leaves(value: unknown, prefix = ''): Map<string, unknown> {
    const result = new Map<string, unknown>()
    for (const [key, entry] of Object.entries(value as object)) {
        const path = prefix ? `${prefix}.${key}` : key
        if (entry && typeof entry === 'object') for (const [child, leaf] of leaves(entry, path)) result.set(child, leaf)
        else result.set(path, entry)
    }
    return result
}

/** Every Korean text by path, with composed messages called on synthetic arguments. */
function koreanTexts(): [string, string][] {
    const texts: [string, string][] = []
    for (const [path, value] of leaves(languageKorean)) {
        let text: unknown = value
        if (typeof value === 'function') {
            try { text = value('Synthetic', 'Synthetic', 'Synthetic') } catch { continue }
        }
        if (typeof text === 'string') texts.push([path, text])
    }
    return texts
}

it('keeps RisuNest and sync Korean and English keys aligned, with Korean text for each', () => {
    for (const section of ['risuNest', 'lwwSync'] as const) {
        const korean = leaves(languageKorean[section])
        expect([...korean.keys()].sort(), section).toEqual([...leaves(languageEnglish[section]).keys()].sort())
        for (const [path, value] of korean) expect(value, `${section}.${path}`).toBeTruthy()
    }
    for (const language of otherLanguages) {
        for (const key of Object.keys(language.lwwSync)) expect(languageEnglish.lwwSync).toHaveProperty(key)
        for (const text of Object.values(language.lwwSync)) expect(text.length).toBeGreaterThan(0)
    }
})

it('composes the upstream restore confirmation in every language', () => {
    for (const language of [languageEnglish, languageKorean, ...otherLanguages]) {
        expect(language.risuNest.importDialog.warningUpstreamRestoreLosses.trim()).not.toBe('')
        const consequence = language.errors.coldStorageIncompleteRestoreConfirm('synthetic', 2, 1, false)
        const defaultConfirmation = language.errors.coldStorageIncompleteRestoreConfirm('synthetic', 2, 1)
        expect(consequence).toContain('synthetic')
        expect(consequence.split('\n\n')).toHaveLength(2)
        expect(defaultConfirmation.startsWith(`${consequence}\n\n`)).toBe(true)
        expect(defaultConfirmation.split('\n\n')).toHaveLength(3)
    }
})

it('uses the required question form for owned Korean confirmations', () => {
    const inherited = new Set(['setup.finally', 'remindLaterQuestion', 'askLoadFirstMsg'])
    for (const [path, text] of koreanTexts()) if (text.includes('까요?')) expect(inherited.has(path), path).toBe(true)
})

it('names values without a particle placeholder in owned Korean text', () => {
    const inherited = new Set(['playground.inlayDeleteConfirm', 'mcpAccessPrompt'])
    for (const [path, text] of koreanTexts()) {
        if (/\((으|이|을|를|은|는|과|와|가)\)|[을은이과와]\([를는가와과]\)/.test(text)) expect(inherited.has(path), path).toBe(true)
    }
})
