import { platform } from '@tauri-apps/plugin-os'
import { isTauri } from '../platform'
import { getNativeDisplayRender } from '../storage/deviceSettings'

/** The shortest message, in UTF-16 units, that renders in the app core. */
const minLength = 65_536

const fenceLine = /^ {0,3}(?:`{3,}|~{3,})/gm

/** At least one fenced code block or `$$` span per 1,000 units. */
function markdownHeavy(text: string): boolean {
    const fences = text.match(fenceLine)?.length ?? 0
    let dollars = 0
    for (let at = text.indexOf('$$'); at !== -1; at = text.indexOf('$$', at + 2)) dollars++
    return (Math.ceil(fences / 2) + Math.floor(dollars / 2)) * 1000 >= text.length
}

/**
 * Whether `text` renders in the app core: a long, Markdown-heavy message does on the platforms where that measured
 * faster than TS, which is Windows desktop. The device setting, when set, decides instead.
 */
export function routesDisplayNatively(text: string): boolean {
    if (!isTauri) return false
    const forced = getNativeDisplayRender()
    if (forced !== null) return forced
    return text.length >= minLength && platform() === 'windows' && markdownHeavy(text)
}
