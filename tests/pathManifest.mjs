import { readFileSync } from 'node:fs'

/** The only modules allowed to name a filesystem root. */
export const APP_MANIFEST = 'src-tauri/src/app_paths.rs'
export const SYNC_MANIFEST = 'server/manager/src/platform/paths.rs'

/**
 * Read the leaf names straight out of the Rust manifest, so this side never
 * becomes a second place a directory name is written down.
 */
export function manifestLeaves(path) {
    const source = readFileSync(path, 'utf8')
    const leaves = new Map()
    for (const [, name, value] of source.matchAll(/^const ([A-Z0-9_]+): &str = "([^"]*)";$/gm))
        leaves.set(name, value)
    for (const [, name, list] of source.matchAll(/^const ([A-Z0-9_]+): \[&str; \d+\] = \[([^\]]*)\];$/gm))
        leaves.set(name, [...list.matchAll(/"([^"]*)"/g)].map(([, entry]) => entry))
    if (leaves.size === 0) throw new Error(`No leaf constants found in ${path}`)
    return leaves
}

function required(leaves, name, path) {
    const value = leaves.get(name)
    if (typeof value !== 'string' || value.length === 0)
        throw new Error(`${path} no longer defines ${name}`)
    return value
}

/**
 * The Windows directories each product owns, plus the directory its installer
 * selects by default. Paths keep the NSIS hive variables so the comparison runs
 * on the same shape the installer script uses.
 */
export function windowsLayout(product) {
    if (product === 'app') {
        const leaves = manifestLeaves(APP_MANIFEST)
        const at = (name) => `$LOCALAPPDATA\\${required(leaves, name, APP_MANIFEST)}`
        return {
            install: at('WINDOWS_INSTALL_LEAF'),
            roots: [
                at('WINDOWS_DATA_LEAF'),
                at('WINDOWS_WEBVIEW_LEAF'),
                at('WINDOWS_CACHE_LEAF'),
                at('WINDOWS_CLEANUP_LEAF'),
            ],
        }
    }
    if (product === 'sync') {
        const leaves = manifestLeaves(SYNC_MANIFEST)
        const at = (name) => `$LOCALAPPDATA\\${required(leaves, name, SYNC_MANIFEST)}`
        return {
            install: at('WINDOWS_INSTALL_LEAF'),
            roots: [at('WINDOWS_DATA_LEAF'), at('WINDOWS_WEBVIEW_LEAF')],
        }
    }
    throw new Error(`Unknown product: ${product}`)
}

/** The directories Tauri derives from an identifier on Windows. */
export function identifierDirectories(identifier) {
    return [`$APPDATA\\${identifier}`, `$LOCALAPPDATA\\${identifier}`]
}

const segments = (path) => path.split(/[\\/]+/).filter((part) => part.length > 0)

/**
 * Component-wise containment. A string prefix would report `RisuNestData` as
 * sitting inside `RisuNest`.
 */
export function overlaps(left, right) {
    const a = segments(left)
    const b = segments(right)
    const shared = Math.min(a.length, b.length)
    for (let index = 0; index < shared; index += 1) {
        if (a[index].toLowerCase() !== b[index].toLowerCase()) return false
    }
    return true
}
