import { vi } from 'vitest'

type ParserModule = typeof import('./parser.svelte')

/**
 * A parser.svelte stand-in for suites that only need its import graph cut: text passes through
 * unchanged and no asset tag matches. Overrides replace single exports and must name real ones.
 */
export function parserStubModule(overrides: { [K in keyof ParserModule]?: unknown } = {}) {
    const stub = {
        assetRegex: /$^/g,
        hasher: vi.fn(async () => 'hash'),
        parseMarkdownSafe: (value: string) => value,
        ParseMarkdown: vi.fn(async (value: string) => value),
        risuChatParser: (value: string) => value,
    } satisfies Partial<ParserModule>
    return { ...stub, ...overrides }
}
