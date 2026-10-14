import { vi } from 'vitest'

// Shared vi.mock module factories that cut AlertComp's import graph for its component tests.
// Consume from a test file as:
//   vi.mock('src/ts/tokenizer', async () => (await import('./AlertComp.testSupport')).tokenizerModule())

type Stores = typeof import('src/ts/stores.svelte')

/** `alertQueue` swaps the plain store for the product dialog queue, for tests that queue dialogs. */
export async function storesModule({ alertQueue = false } = {}) {
    const { writable } = await import('svelte/store')
    const initial = { type: 'none', msg: '' } as const
    const alertStore = alertQueue
        ? (await import('src/ts/alertQueue')).createAlertQueue(initial, { gapMs: 0 })
        : writable(initial) as unknown as Stores['alertStore']
    return {
        DBState: { db: { characters: [], botPresets: [], botPresetsId: 0 } } as unknown as Stores['DBState'],
        alertStore,
        selectedCharID: writable(-1),
    } satisfies Partial<Stores>
}

export async function alertModule() {
    const { writable } = await import('svelte/store')
    return { alertGenerationInfoStore: writable(null) } satisfies Partial<typeof import('src/ts/alert')>
}

export function charactersModule() {
    return { getCharImage: vi.fn() } satisfies Partial<typeof import('src/ts/characters')>
}

export function characterCardsModule() {
    return { hubURL: '', isCharacterHasAssets: () => false } satisfies Partial<typeof import('src/ts/characterCards')>
}

export function tokenizerModule() {
    return { tokenize: vi.fn(async (text: string) => text.length) } satisfies Partial<typeof import('src/ts/tokenizer')>
}

export async function colorschemeModule() {
    const { writable } = await import('svelte/store')
    return { ColorSchemeTypeStore: writable<'dark' | 'light'>('dark') } satisfies Partial<typeof import('src/ts/gui/colorscheme')>
}

export function sourcemapModule() {
    return { translateStackTrace: vi.fn() } satisfies Partial<typeof import('src/ts/sourcemap')>
}

export function officialAccountMessageModule() {
    return { isExpectedHubMessage: () => false } satisfies Partial<typeof import('src/ts/storage/officialAccountMessage')>
}

export async function lucideModule() {
    const Stub = (await import('./AlertCompDependencyStub.test.svelte')).default as unknown as typeof import('@lucide/svelte')['XIcon']
    return {
        CheckIcon: Stub,
        ChevronDownIcon: Stub,
        ChevronRightIcon: Stub,
        ChevronUpIcon: Stub,
        CopyIcon: Stub,
        User: Stub,
        XIcon: Stub,
    } satisfies Partial<typeof import('@lucide/svelte')>
}

/** A Svelte component module that renders nothing; the button stub renders its children and forwards clicks. */
export async function componentModule(kind: 'empty' | 'button' = 'empty') {
    const stub = kind === 'button'
        ? await import('./AlertCompButtonStub.test.svelte')
        : await import('./AlertCompDependencyStub.test.svelte')
    return { default: stub.default }
}
