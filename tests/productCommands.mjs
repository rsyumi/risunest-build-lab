import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

// Product code calls native commands by name, which type checks cannot see.
const root = fileURLToPath(new URL('..', import.meta.url))
const callerRoots = ['src/ts', 'src/lib']
const testSource = /\.d\.ts$|\.(?:test|spec)\.|\.bench\.ts$|\.testSupport\.ts$|\.testUtils\.ts$|(?:^|\/)(?:tests|__tests__|__fixtures__|test-fixtures)\/|realmEndpoints\.blocked\.ts$/

/**
 * The call shapes that name a command. A command reached any other way (a variable
 * name, an interpolated string) is not checked.
 */
export const callerPatterns = [
    { id: 'invoke', shape: "invoke('x') and receiver.invoke('x'), including injected invoke dependencies", callee: /(?<![\w$])invoke(?![\w$])/g },
    { id: 'invokeCommand', shape: "invokeCommand('x'), an injected invoke such as nativeLog.ts takes", callee: /(?<![\w$.])invokeCommand(?![\w$])/g },
    { id: 'invokeStore', shape: "invokeStore('x') in the persistent store", callee: /(?<![\w$.])invokeStore(?![\w$])/g },
    { id: 'invokeNative', shape: "invokeNative(dependencies, 'x') in native file jobs", callee: /(?<![\w$.])invokeNative(?![\w$])/g, afterArgument: true },
    { id: 'invokeJob', shape: "invokeJob(dependencies, 'x') in native backups", callee: /(?<![\w$.])invokeJob(?![\w$])/g, afterArgument: true },
    { id: 'invokeArchiveOperation', shape: "invokeArchiveOperation('x') in the persistent store", callee: /(?<![\w$.])invokeArchiveOperation(?![\w$])/g },
    { id: 'protectedCommand', shape: "protectedCommand('x') in server asset residency", callee: /(?<![\w$.])protectedCommand(?![\w$])/g },
    { id: 'this.native', shape: "this.native('x') in the external storage bridge", callee: /(?<![\w$.])this\.native(?![\w$])/g },
    { id: 'command', shape: "command('x') in image geometry", callee: /(?<![\w$.])command(?![\w$])/g, file: 'src/ts/storage/imageGeometry.ts' },
    { id: 'stage', shape: "stage('x') in the persistent store", callee: /(?<![\w$.])stage(?![\w$])/g, file: 'src/ts/storage/sqlitePersistentDataStore.ts' },
    { id: 'read', shape: "read('x') in the platform blob store", callee: /(?<![\w$.])read(?![\w$])/g, file: 'src/ts/storage/platformBlobStore.ts' },
]
/** Commands handed to a caller as a value: `handoffCleanupCommand: 'x'`, `directCommand: 'x'`, `X_COMMAND = 'x'`. */
export const commandFieldPattern = {
    id: 'command field',
    shape: "fooCommand: 'x' and FOO_COMMAND = 'x'",
    pattern: /(?<![\w$])(?:\w*Command\s*:|[A-Z][A-Z0-9_]*_COMMAND\s*=)\s*(['"])([a-z][a-z0-9]*(?:_[a-z0-9]+)+)\1/g,
}

const commandLiteral = /^\s*(['"`])([a-z][a-z0-9_]*)\1/
const leadingArgument = /^\s*[A-Za-z_$][\w$.]*\s*,/

/** The index just past the opening parenthesis of a call, skipping type arguments, or -1. */
function callArguments(source, index) {
    while (/\s/.test(source[index] ?? '')) index++
    if (source[index] === '<') {
        let depth = 0
        const limit = Math.min(source.length, index + 400)
        for (; index < limit; index++) {
            if (source[index] === '<') depth++
            else if (source[index] === '>' && source[index - 1] !== '=' && --depth === 0) break
        }
        if (depth !== 0) return -1
        index++
        while (/\s/.test(source[index] ?? '')) index++
    }
    return source[index] === '(' ? index + 1 : -1
}

const lineOf = (source, index) => source.slice(0, index).split('\n').length

/** Every command name a product source passes to a recognized caller. */
export function commandCalls(file, source) {
    const calls = []
    for (const pattern of callerPatterns) {
        if (pattern.file && pattern.file !== file) continue
        for (const match of source.matchAll(pattern.callee)) {
            let index = callArguments(source, match.index + match[0].length)
            if (index < 0) continue
            if (pattern.afterArgument) {
                const argument = source.slice(index).match(leadingArgument)
                if (!argument) continue
                index += argument[0].length
            }
            const literal = source.slice(index).match(commandLiteral)
            if (literal) calls.push({ command: literal[2], file, line: lineOf(source, match.index), pattern: pattern.id })
        }
    }
    for (const match of source.matchAll(commandFieldPattern.pattern)) {
        calls.push({ command: match[2], file, line: lineOf(source, match.index), pattern: commandFieldPattern.id })
    }
    return calls
}

/** The command names in the product `invoke_handler()` router, in order and with repeats. */
export function productRegistry(libSource) {
    const start = libSource.indexOf('pub fn invoke_handler()')
    if (start < 0) return null
    const open = libSource.indexOf('generate_handler![', start)
    if (open < 0) return null
    let depth = 1
    let end = open + 'generate_handler!['.length
    for (; depth > 0; end++) {
        if (end >= libSource.length) throw new Error('Unterminated generate_handler!')
        if (libSource[end] === '[') depth++
        else if (libSource[end] === ']') depth--
    }
    return libSource.slice(open + 'generate_handler!['.length, end - 1)
        .replace(/#\[[^\]]*\]/g, '')
        .split(',')
        .map((path) => path.trim().split('::').at(-1))
        .filter(Boolean)
}

export function checkProductCommands(libSource, sources) {
    const registered = productRegistry(libSource) ?? []
    const seen = new Set()
    const duplicates = registered.filter((name) => seen.has(name) || !seen.add(name))
    const calls = sources.flatMap(({ file, source }) => commandCalls(file, source))
    const matched = new Set(calls.map((call) => call.pattern))
    return {
        router: registered.length > 0,
        // The builder must install the product router, not only define it.
        linked: /\.invoke_handler\(\s*invoke_handler\(\s*\)\s*\)/.test(libSource),
        duplicates,
        missing: calls.filter((call) => !seen.has(call.command)),
        unmatchedPatterns: [...callerPatterns.map((pattern) => pattern.id), commandFieldPattern.id]
            .filter((id) => !matched.has(id)),
        calls,
    }
}

function* sourceFiles(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
        const path = join(directory, entry.name)
        if (entry.isDirectory()) yield* sourceFiles(path)
        else if (/\.(?:ts|svelte)$/.test(entry.name)) yield path
    }
}

export function readProductSources(base = root) {
    const sources = []
    for (const directory of callerRoots) {
        for (const path of sourceFiles(join(base, directory))) {
            const file = relative(base, path).replaceAll('\\', '/')
            if (!testSource.test(file)) sources.push({ file, source: readFileSync(path, 'utf8') })
        }
    }
    return sources
}

export function readProductRouter(base = root) {
    return readFileSync(join(base, 'src-tauri/src/lib.rs'), 'utf8')
}
