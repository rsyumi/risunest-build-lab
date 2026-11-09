import { Buffer } from 'buffer'
import { describe, expect, it } from 'vitest'
import {
    classifyChatParserHistory,
    planChatParserHistory,
    extendChatParserHistoryBounds,
    type ChatParserUnsafeHistoryDependency,
} from './chatParserHistory'

describe('chat parser history classification', () => {
    it.each([
        '{{#if 1}}HP{{/if}}', '{{#when::1::is::1}}HP{{/when}}',
        '{{#pure}}HP{{/pure}}', '{{#escape}}HP{{/escape}}',
        '{{#pure}}{{history}}{{/pure}}', '{{#escape}}{{history}}{{/escape}}',
        '{{#if 1}}{{previouschatlog::7}}{{/if}}',
        '{{#if {{getvar::hp}}}}HP{{/if}}', '{{#if {{? {{getvar::hp}}>0}}}}HP{{/if}}',
        '{{#if {{equal::{{getvar::x}}::1}}}}HP{{/if}}', '{{#when {{getvar::x}}}}HP{{/when}}',
        '{{#if_pure {{getvar::x}}}}HP{{/if_pure}}', 'HP {{? {{getvar::hp}}+1}}',
    ])('keeps statically visible blocks bounded: %s', (source) => {
        expect(classifyChatParserHistory({ source }).requiresFullHistory).toBe(false)
        expect(planChatParserHistory({ source }).requiresFullHistory).toBe(false)
    })

    it.each([
        '{{#each {{array::a::b}} as item}}x{{/each}}', '{{call::f}}', '{{{{getvar::m}}}}',
        '{{last message{{getvar::empty}}}}', '{{#{{getvar::block}} 1}}x{{/if}}', '{{?{{getvar::x}}}}',
    ])('keeps text-splicing expressions opaque: %s', (source) => {
        expect(planChatParserHistory({ source }).reasons).toContain('opaque-cbs')
    })
    it.each([
        '{{user-history}}',
        '{{CHAR_HISTORY}}',
        '{{message-unixtime-array}}',
        '{{idle duration}}',
        '{{last_message_index}}',
        '{{roll-pick::1d6}}',
    ])('normalizes full-history CBS aliases in %s', (source) => {
        expect(classifyChatParserHistory({ source })).toMatchObject({
            requiresFullHistory: true,
            reasons: ['full-history-cbs'],
        })
    })

    it('follows recursive CBS indirection before choosing a bounded projection', () => {
        const result = classifyChatParserHistory({
            source: '{{personality}}',
            indirections: {
                personality: '{{history}}',
            },
        })

        expect(result.requiresFullHistory).toBe(true)
        expect(result.reasons).toContain('full-history-cbs')
    })

    it('extends a bounded projection for literal previous-chat-log indices', () => {
        const classification = classifyChatParserHistory({
            source: '{{previous-chat-log :: 10}} {{previous_chat_log::95}}',
        })

        expect(classification).toEqual({
            requiresFullHistory: false,
            absoluteMessageIndices: [10, 95],
            reasons: [],
        })
        expect(
            extendChatParserHistoryBounds(classification, {
                start: 89,
                end: 90,
                totalMessages: 100,
            }),
        ).toEqual({ start: 10, end: 96 })
    })

    it('requires full history when a previouschatlog index is dynamic', () => {
        expect(
            classifyChatParserHistory({
                source: '{{previouschatlog::{{getvar::target}}}}',
            }),
        ).toMatchObject({
            requiresFullHistory: true,
            reasons: ['dynamic-previous-chat-log'],
        })
    })

    it('classifies CBS hidden in decoded risu-style CSS', () => {
        const encodedCss = Buffer.from('.turn::after{content:"{{history}}"}').toString('hex')

        expect(
            classifyChatParserHistory({
                source: `<risu-style>${encodedCss}</risu-style>`,
            }),
        ).toMatchObject({
            requiresFullHistory: true,
            reasons: ['full-history-cbs'],
        })
    })

    it('keeps known-safe risu-style CSS bounded and falls back on ambiguous encoding', () => {
        const safeCss = Buffer.from('.turn{color:red}').toString('hex')
        expect(
            classifyChatParserHistory({
                source: `<risu-style>${safeCss}</risu-style>`,
            }),
        ).toMatchObject({ requiresFullHistory: false, reasons: [] })
        expect(
            classifyChatParserHistory({
                source: '<risu-style>not-hex</risu-style>',
            }),
        ).toMatchObject({
            requiresFullHistory: true,
            reasons: ['ambiguous-risu-style'],
        })
    })

    it.each<ChatParserUnsafeHistoryDependency>([
        'lua',
        'plugin-v2',
        'display-trigger',
        'inject',
    ])('requires full history for unsafe %s processing', (dependency) => {
        expect(
            classifyChatParserHistory({
                source: 'ordinary text',
                unsafeDependencies: [dependency],
            }),
        ).toEqual({
            requiresFullHistory: true,
            absoluteMessageIndices: [],
            reasons: [dependency],
        })
    })

    it('collects parser input strings recursively without following cycles', () => {
        const source: { nested: unknown; self?: unknown } = {
            nested: [{ value: '{{previouschatlog::3}}' }],
        }
        source.self = source

        expect(classifyChatParserHistory({ source })).toEqual({
            requiresFullHistory: false,
            absoluteMessageIndices: [3],
            reasons: [],
        })
    })

    it('collects the strings of a conversation with hundreds of thousands of fields', () => {
        const message = Array.from({ length: 300_000 }, (_, index) => ({ role: 'user', data: `#${index}` }))
        message.push({ role: 'char', data: '{{previouschatlog::7}}' })

        expect(classifyChatParserHistory({ source: [{ message }] })).toEqual({
            requiresFullHistory: false,
            absoluteMessageIndices: [7],
            reasons: [],
        })
    })

    it('ignores requested literal indices outside the available conversation', () => {
        const classification = classifyChatParserHistory({
            source: '{{previouschatlog::500}}',
        })

        expect(
            extendChatParserHistoryBounds(classification, {
                start: 89,
                end: 90,
                totalMessages: 100,
            }),
        ).toEqual({ start: 89, end: 90 })
    })
})
