import markdownit from 'markdown-it'

export type ReleaseNoteInline =
    | { type: 'text'; text: string }
    | { type: 'strong'; text: string }
    | { type: 'em'; text: string }
    | { type: 'code'; text: string }
    | { type: 'link'; text: string; url: string }

export type ReleaseNoteBlock =
    | { type: 'heading'; level: number; content: ReleaseNoteInline[] }
    | { type: 'paragraph'; content: ReleaseNoteInline[] }
    // An empty marker continues the previous item (a second paragraph inside it).
    | { type: 'list-item'; depth: number; marker: string; content: ReleaseNoteInline[] }

type Token = ReturnType<ReturnType<typeof markdownit>['parse']>[number]

const NOTE_LIMIT_BYTES = 4096

// Notes arrive from the signed manifest and are rendered as Svelte elements, never as HTML.
// The zero preset starts with paragraphs and plain text only; everything else stays literal
// unless enabled here. HTML and tables are intentionally absent, images reduce to their
// alt text and fenced code becomes a plain code paragraph.
const md = markdownit('zero').enable([
    'heading',
    'list',
    'hr',
    'blockquote',
    'fence',
    'newline',
    'escape',
    'entity',
    'backticks',
    'emphasis',
    'link',
    'image',
])

export function selectLocalizedNotes(
    localized: Record<string, string>,
    locale: string,
    fallback: string,
): string {
    const normalized = locale.toLowerCase()
    const base = normalized.split('-')[0]
    const entries = Object.entries(localized)
    const selected = localized[locale]
        ?? entries.find(([key]) => key.toLowerCase() === normalized)?.[1]
        ?? entries.find(([key]) => key.toLowerCase() === base)?.[1]
        ?? localized.en
        ?? entries.find(([key]) => key.toLowerCase() === 'en')?.[1]
        ?? entries[0]?.[1]
        ?? fallback
        ?? ''
    return truncateAtLineBoundary(selected, NOTE_LIMIT_BYTES)
}

export function parseReleaseNotes(markdown: string): ReleaseNoteBlock[] {
    const blocks: ReleaseNoteBlock[] = []
    const lists: { ordered: boolean; next: number }[] = []
    let itemStarted = false
    let heading = 0
    for (const token of md.parse(markdown, {})) {
        switch (token.type) {
            case 'heading_open':
                heading = Number(token.tag.slice(1)) || 1
                break
            case 'heading_close':
                heading = 0
                break
            case 'bullet_list_open':
                lists.push({ ordered: false, next: 0 })
                break
            case 'ordered_list_open':
                lists.push({ ordered: true, next: Number(token.attrGet('start') ?? '1') || 1 })
                break
            case 'bullet_list_close':
            case 'ordered_list_close':
                lists.pop()
                break
            case 'list_item_open':
                itemStarted = true
                break
            case 'fence': {
                const code = token.content.trimEnd()
                if (code) blocks.push({ type: 'paragraph', content: [{ type: 'code', text: code }] })
                break
            }
            case 'inline': {
                const content = parseInline(token.children ?? [])
                if (!content.length) break
                const list = lists.at(-1)
                if (list) {
                    let marker = ''
                    if (itemStarted) {
                        marker = list.ordered ? `${list.next}.` : '•'
                        if (list.ordered) list.next += 1
                        itemStarted = false
                    }
                    blocks.push({ type: 'list-item', depth: lists.length, marker, content })
                } else if (heading) {
                    blocks.push({ type: 'heading', level: heading, content })
                } else {
                    blocks.push({ type: 'paragraph', content })
                }
                break
            }
        }
    }
    return blocks
}

function parseInline(children: Token[]): ReleaseNoteInline[] {
    const tokens: ReleaseNoteInline[] = []
    let strong = 0
    let em = 0
    let link: { url: string; text: string } | null = null
    const push = (token: ReleaseNoteInline) => {
        const last = tokens.at(-1)
        if (last && last.type !== 'link' && last.type === token.type) last.text += token.text
        else tokens.push(token)
    }
    const text = (value: string) => {
        if (link) link.text += value
        else push({ type: strong ? 'strong' : em ? 'em' : 'text', text: value })
    }
    for (const child of children) {
        switch (child.type) {
            case 'text':
                text(child.content)
                break
            case 'softbreak':
            case 'hardbreak':
                text(' ')
                break
            case 'code_inline':
                if (link) link.text += child.content
                else push({ type: 'code', text: child.content })
                break
            case 'image':
                text(child.content)
                break
            case 'strong_open': strong += 1; break
            case 'strong_close': strong -= 1; break
            case 'em_open': em += 1; break
            case 'em_close': em -= 1; break
            case 'link_open':
                link = { url: child.attrGet('href') ?? '', text: '' }
                break
            case 'link_close':
                if (link) {
                    if (isSafeLink(link.url)) tokens.push({ type: 'link', text: link.text, url: link.url })
                    else push({ type: strong ? 'strong' : em ? 'em' : 'text', text: link.text })
                }
                link = null
                break
        }
    }
    if (link) {
        const pending = link.text
        link = null
        text(pending)
    }
    return tokens.filter(token => token.text)
}

function isSafeLink(value: string): boolean {
    try {
        const url = new URL(value)
        return url.protocol === 'https:' && !url.username && !url.password
    } catch {
        return false
    }
}

function truncateAtLineBoundary(value: string, maxBytes: number): string {
    if (new TextEncoder().encode(value).byteLength <= maxBytes) return value
    const lines = value.replace(/\r\n?/g, '\n').split('\n')
    let result = ''
    for (const line of lines) {
        const candidate = result ? `${result}\n${line}` : line
        if (new TextEncoder().encode(candidate).byteLength > maxBytes) break
        result = candidate
    }
    return result
}
