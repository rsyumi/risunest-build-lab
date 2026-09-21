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

test('the manifests are the modules that do resolve roots', async () => {
    const app = await readFile(join(ROOT, APP_MANIFEST.split('/').join(sep)), 'utf8')
    assert.match(app, /\bdirs::data_local_dir\(\)/)
    const sync = await readFile(join(ROOT, SYNC_MANIFEST.split('/').join(sep)), 'utf8')
    assert.match(sync, /"LOCALAPPDATA"/)
})

test('the iOS staging root is received from Rust rather than derived in Swift', async () => {
    const swift = await readFile(
        join(ROOT, 'crates', 'tauri-plugin-ios-native', 'ios', 'Sources', 'IosNativePlugin.swift'),
        'utf8',
    )
    // Swift enforces file ownership against this root, so a derivation of its
    // own would make the check verify nothing.
    assert.doesNotMatch(swift, /applicationSupportDirectory/)
    assert.match(swift, /private var dataRoot: URL\?/)
    assert.match(swift, /func setDataRoot\(/)
    // The sweep may only run once the root has arrived.
    assert.doesNotMatch(swift, /webView = webview\s+sweepStaging\(/)

    const plugin = await readFile(
        join(ROOT, 'crates', 'tauri-plugin-ios-native', 'src', 'lib.rs'),
        'utf8',
    )
    assert.match(plugin, /run_mobile_plugin_async::<\(\)>\(\s*"setDataRoot"/)
    const entry = await readFile(join(ROOT, 'src-tauri', 'src', 'lib.rs'), 'utf8')
    assert.match(entry, /set_data_root\(&root\)/)

    // Both consumers name the same leaf inside that root.
    const helper = await readFile(join(ROOT, 'src', 'ts', 'storage', 'nativePaths.ts'), 'utf8')
    for (const source of [swift, helper]) assert.match(source, /'ios-file-staging'|"ios-file-staging"/)
})

test('orphan asset collection is left to the native repository', async () => {
    const bootstrap = await readFile(join(ROOT, 'src', 'ts', 'bootstrap.ts'), 'utf8')
    // The renderer enumerated a flat assets directory that the
    // content-addressed store no longer has. Reclamation lives in
    // src-tauri/src/asset_repository, which owns the job pins and GC roots.
    assert.doesNotMatch(bootstrap, /readDir\(\s*'assets'/)
    assert.doesNotMatch(bootstrap, /blobStore\.remove\(\s*'assets\/'/)
    for (const name of ['job_pins.rs', 'migration_gc.rs']) {
        const module = await readFile(join(ROOT, 'src-tauri', 'src', 'asset_repository', name), 'utf8')
        assert.match(module, /AssetRootSet/)
    }
    const store = await readFile(join(ROOT, 'src-tauri', 'src', 'persistent_store', 'mod.rs'), 'utf8')
    assert.match(store, /fn recover_asset_object_deletions/)
})
