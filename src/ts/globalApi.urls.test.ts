import { afterEach, expect, test, vi } from 'vitest'
import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { open } from '@tauri-apps/plugin-shell'
import { openUrl } from '@tauri-apps/plugin-opener'

const native = { invoke: vi.fn().mockResolvedValue(undefined) }

const originalInternals = Object.getOwnPropertyDescriptor(window, '__TAURI_INTERNALS__')
const originalUserAgent = Object.getOwnPropertyDescriptor(navigator, 'userAgent')
const errors: string[] = []
const url = 'https://github.com/rsyumi/RisuNest/releases'

async function loadOpenURL(userAgent: string, tauri = true): Promise<(url: string) => void> {
    Object.defineProperty(navigator, 'userAgent', { configurable: true, value: userAgent })
    Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: tauri ? { invoke: native.invoke } : undefined })
    const platform = await import('./platform')
    // Execute the facade's actual function without starting unrelated application effects.
    const source = ts.createSourceFile('globalApi.svelte.ts', readFileSync('src/ts/globalApi.svelte.ts', 'utf8'), ts.ScriptTarget.Latest, true)
    const declaration = source.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'openURL')
    if (!declaration) throw new Error('Missing openURL facade')
    const body = ts.transpileModule(declaration.getText(source), {
        compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText
    return new Function('exports', 'isTauri', 'isTauriIOS', 'isTauriMobile', 'open', 'openUrl', 'alertError', 'window', `${body}; return exports.openURL;`)(
        {}, platform.isTauri, platform.isTauriIOS, platform.isTauriMobile, open, openUrl, (message: string) => errors.push(message), window,
    )
}

afterEach(() => {
    for (const [target, key, descriptor] of [
        [window, '__TAURI_INTERNALS__', originalInternals],
        [navigator, 'userAgent', originalUserAgent],
    ] as const) {
        if (descriptor) Object.defineProperty(target, key, descriptor)
        else Reflect.deleteProperty(target, key)
    }
    vi.restoreAllMocks()
    native.invoke.mockReset().mockResolvedValue(undefined)
    errors.length = 0
    vi.resetModules()
})

test.each([
    ['Android', 'Mozilla/5.0 (Linux; Android 14)', 'plugin:opener|open_url'],
    ['iOS', 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)', 'plugin:opener|open_url'],
    ['Windows', 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)', 'plugin:shell|open'],
    ['macOS', 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15)', 'plugin:shell|open'],
    ['Linux', 'Mozilla/5.0 (X11; Linux x86_64)', 'plugin:shell|open'],
])('%s sends external URLs to the platform URL handler', async (_name, userAgent, command) => {
    const openURL = await loadOpenURL(userAgent)
    openURL(url)
    expect(native.invoke).toHaveBeenCalledWith(command, command === 'plugin:opener|open_url'
        ? { url, with: undefined }
        : { path: url, with: undefined }, undefined)
})

test.each(['Mozilla/5.0 (Linux; Android 14)', 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)'])(
    'browser URLs open in a new tab (%s)', async (userAgent) => {
        const openURL = await loadOpenURL(userAgent, false)
        const browserOpen = vi.spyOn(window, 'open').mockReturnValue(null)
        openURL(url)
        expect(browserOpen).toHaveBeenCalledWith(url, '_blank')
        expect(native.invoke).not.toHaveBeenCalled()
    },
)

test.each([
    ['Android', 'Mozilla/5.0 (Linux; Android 14)', 'No activity can open this URL'],
    ['Linux', 'Mozilla/5.0 (X11; Linux x86_64)', 'Scoped shell IO error: No such file or directory (os error 2)'],
])('%s reports a failed native URL handoff', async (_name, userAgent, failure) => {
    const openURL = await loadOpenURL(userAgent)
    native.invoke.mockRejectedValueOnce(failure)
    openURL(url)
    await vi.waitFor(() => expect(errors).toEqual([failure]))
})
