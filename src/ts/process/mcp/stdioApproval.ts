import { alertConfirm } from '../../alert'
import { language } from 'src/lang'
import { getDeviceMarkers } from '../../storage/deviceMarkers'

export interface LocalMCPConfiguration { command: string; args: string | string[]; env?: Record<string, string> }
const declined = new Set<string>()
const pending = new Map<string, Promise<boolean>>()

export async function approveLocalMCP(config: LocalMCPConfiguration): Promise<boolean> {
    const canonical = { command: config.command, args: Array.isArray(config.args) ? config.args : [config.args], env: Object.fromEntries(Object.entries(config.env ?? {}).sort(([a], [b]) => a.localeCompare(b))) }
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(JSON.stringify(canonical)))
    const key = Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, '0')).join('')
    const markers = getDeviceMarkers()
    const stored: unknown = JSON.parse(markers.getItem('mcpStdioApprovals') ?? '[]')
    const approvals = new Set(Array.isArray(stored) ? stored.filter((value): value is string => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value)) : [])
    if (approvals.has(key)) return true
    if (declined.has(key)) return false
    const existing = pending.get(key)
    if (existing) return existing
    const approval = (async () => {
        if (!await alertConfirm(language.mcpStdioRegisterConfirm + '\n\n' + JSON.stringify(config, null, 2))) {
            declined.add(key)
            return false
        }
        const latest: unknown = JSON.parse(markers.getItem('mcpStdioApprovals') ?? '[]')
        const merged = new Set(Array.isArray(latest) ? latest.filter(value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value)) : [])
        merged.add(key)
        markers.setItem('mcpStdioApprovals', JSON.stringify([...merged]))
        await markers.flush()
        return true
    })().finally(() => pending.delete(key))
    pending.set(key, approval)
    return approval
}
