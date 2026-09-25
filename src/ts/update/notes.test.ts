import { describe, expect, it } from 'vitest'
import { parseReleaseNotes, selectLocalizedNotes } from './notes'

describe('release notes', () => {
    it('selects locale, English, first locale, then default notes', () => {
        const notes = { en: 'English', ko: '한국어' }
        expect(selectLocalizedNotes(notes, 'ko-KR', 'Default')).toBe('한국어')
        expect(selectLocalizedNotes(notes, 'de', 'Default')).toBe('English')
        expect(selectLocalizedNotes({ fr: 'Français' }, 'de', 'Default')).toBe('Français')
        expect(selectLocalizedNotes({}, 'de', 'Default')).toBe('Default')
    })

    it('parses headings, paragraphs and inline emphasis', () => {
        expect(parseReleaseNotes('## Changes\n### Fixed\nPlain **bold** *em* `code`\nnext line')).toEqual([
            { type: 'heading', level: 2, content: [{ type: 'text', text: 'Changes' }] },
            { type: 'heading', level: 3, content: [{ type: 'text', text: 'Fixed' }] },
            { type: 'paragraph', content: [
                { type: 'text', text: 'Plain ' },
                { type: 'strong', text: 'bold' },
                { type: 'text', text: ' ' },
                { type: 'em', text: 'em' },
                { type: 'text', text: ' ' },
                { type: 'code', text: 'code' },
                { type: 'text', text: ' next line' },
            ] },
        ])
    })

    it('numbers ordered items, nests lists and keeps continuation paragraphs', () => {
        const blocks = parseReleaseNotes('3. three\n4. four\n- bullet\n  wrapped\n\n  second paragraph\n  - nested\n    - deeper')
        expect(blocks).toEqual([
            { type: 'list-item', depth: 1, marker: '3.', content: [{ type: 'text', text: 'three' }] },
            { type: 'list-item', depth: 1, marker: '4.', content: [{ type: 'text', text: 'four' }] },
            { type: 'list-item', depth: 1, marker: '•', content: [{ type: 'text', text: 'bullet wrapped' }] },
            { type: 'list-item', depth: 1, marker: '', content: [{ type: 'text', text: 'second paragraph' }] },
            { type: 'list-item', depth: 2, marker: '•', content: [{ type: 'text', text: 'nested' }] },
            { type: 'list-item', depth: 3, marker: '•', content: [{ type: 'text', text: 'deeper' }] },
        ])
    })

    it('keeps https links only and flattens their contents to text', () => {
        const blocks = parseReleaseNotes('[safe](https://example.com/a_(b) "title") [http](http://example.com) [bad](javascript:alert(1)) [user](https://u:p@example.com)')
        expect(blocks).toEqual([{ type: 'paragraph', content: [
            { type: 'link', text: 'safe', url: 'https://example.com/a_(b)' },
            { type: 'text', text: ' http [bad](javascript:alert(1)) user' },
        ] }])
    })

    it('keeps HTML as text and reduces images, fences and rules to text', () => {
        const blocks = parseReleaseNotes('<script>run()</script>\n\n![alt text](https://example.com/i.png)\n\n---\n\n```\nline 1\nline 2\n```\n\n> quoted')
        expect(blocks).toEqual([
            { type: 'paragraph', content: [{ type: 'text', text: '<script>run()</script>' }] },
            { type: 'paragraph', content: [{ type: 'text', text: 'alt text' }] },
            { type: 'paragraph', content: [{ type: 'code', text: 'line 1\nline 2' }] },
            { type: 'paragraph', content: [{ type: 'text', text: 'quoted' }] },
        ])
    })

    it('truncates oversized localized notes at a line boundary', () => {
        const value = `${'a'.repeat(3000)}\n${'b'.repeat(3000)}`
        expect(selectLocalizedNotes({ en: value }, 'en', '')).toBe('a'.repeat(3000))
    })
})
