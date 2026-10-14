import assert from 'node:assert/strict'
import { readdir, readFile } from 'node:fs/promises'
import { join, posix, relative, sep } from 'node:path'
import test from 'node:test'
import { APP_MANIFEST, SYNC_MANIFEST } from './pathManifest.mjs'

const ROOT = new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')

/** Product source only. Vendored crates and generated trees are not ours. */
const SCANNED = [
    'src',
    'src-tauri/src',
    'server/manager/src',
    'server/manager/gui/src',
    'server/manager/gui/src-tauri/src',
]

/**
 * The manifests own every root, and the two entries that have to resolve one
 * before a manifest exists.
 */
const EXEMPT = new Set(
    [
        APP_MANIFEST,
        'src-tauri/src/app_paths/tests.rs',
        SYNC_MANIFEST,
        // An alternative native entry supplies its own roots, so it names the
        // one directory it was handed rather than deriving anything.
        'src/ts/storage/nativePaths.ts',
    ].map((path) => path.split('/').join(sep)),
)

/**
 * Each pattern is a way to derive a filesystem root outside the manifest. The
 * Windows uninstall failure came from eight of these disagreeing, so a ninth
 * must fail the build rather than appear quietly.
 */
const FORBIDDEN = [
    { pattern: /\bdirs::(data_dir|data_local_dir|config_dir|cache_dir|home_dir)\b/, extensions: ['.rs'] },
    { pattern: /\.path\(\)\s*\.\s*app_(data_dir|config_dir|local_data_dir|cache_dir|log_dir)\b/, extensions: ['.rs'] },
    // The hive name itself, not one call shape: a closure around var_os would
    // otherwise slip through.
    { pattern: /"(APPDATA|LOCALAPPDATA|XDG_DATA_HOME|XDG_CONFIG_HOME|XDG_CACHE_HOME)"/, extensions: ['.rs'] },
    { pattern: /\bBaseDirectory\.App(Data|Config|LocalData|Cache)\b/, extensions: ['.ts', '.svelte'] },
    { pattern: /\bapp(DataDir|ConfigDir|LocalDataDir|CacheDir)\s*\(/, extensions: ['.ts', '.svelte'] },
]

async function sources() {
    const found = []
    for (const directory of SCANNED) {
        const absolute = join(ROOT, directory.split('/').join(sep))
        const entries = await readdir(absolute, { recursive: true, withFileTypes: true })
        for (const entry of entries) {
            if (!entry.isFile()) continue
            const path = join(entry.parentPath ?? entry.path, entry.name)
            found.push(relative(ROOT, path))
        }
    }
    assert.ok(found.length > 500, `expected a full source scan, saw ${found.length} files`)
    return found
}

test('no module outside the path manifests derives a filesystem root', async () => {
    const offences = []
    for (const path of await sources()) {
        if (EXEMPT.has(path)) continue
        if (path.includes('.test.') || path.endsWith('.bench.ts')) continue
        const applicable = FORBIDDEN.filter(({ extensions }) =>
            extensions.some((extension) => path.endsWith(extension)),
        )
        if (applicable.length === 0) continue
        const source = await readFile(join(ROOT, path), 'utf8')
        for (const { pattern } of applicable) {
            const match = source.match(pattern)
            if (match) offences.push(`${path.split(sep).join(posix.sep)}: ${match[0]}`)
        }
    }
    assert.deepEqual(offences, [])
})

test('every native SQLite open names its database through the shared helper', async () => {
    // On Windows SQLite takes the locks of a database named by a \\?\ path
    // through one handle for the whole process, which can strand a read lock,
    // so every open goes through the helper that names it by its drive path.
    const native = join('src-tauri', 'src') + sep
    const helper = join('src-tauri', 'src', 'sqlite_open.rs')
    const offences = []
    let scanned = 0
    for (const path of await sources()) {
        if (!path.startsWith(native) || !path.endsWith('.rs') || path === helper) continue
        if (/(^|[\\/_])tests\.rs$/.test(path) || path.includes(`${sep}tests${sep}`)) continue
        const source = await readFile(join(ROOT, path), 'utf8')
        const product = source.split(/^#\[cfg\(test\)\]\r?\n(?:pub(?:\([^)]*\))? )?mod \w+ \{/m)[0]
        scanned += 1
        for (const match of product.matchAll(/\bConnection::open(?:_with_flags)?(?:_and_vfs)?\s*\(/g)) {
            const line = product.slice(0, match.index).split('\n').length
            offences.push(`${path.split(sep).join(posix.sep)}:${line}`)
        }
    }
    assert.ok(scanned > 100, `expected a full native source scan, saw ${scanned} files`)
    assert.deepEqual(offences, [])
})
