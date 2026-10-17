import { readable } from 'svelte/store'
import { invoke } from '@tauri-apps/api/core'
import { arch } from '@tauri-apps/plugin-os'
import { getDetailedOSLabel, getFallbackOSLabel, isTauri } from './platform'
import { appVer } from './appVersion'
import versionData from '../../version.json'

export type DiagnosticInfo = { os: string; device: string; userAgent: string }

function fallbackInfo(): DiagnosticInfo {
    let device = 'Unknown'
    try { if (isTauri) device = arch() } catch { /* Native startup may have failed. */ }
    return {
        os: getFallbackOSLabel(),
        device,
        userAgent: typeof navigator === 'undefined' ? 'Unknown' : navigator.userAgent || 'Unknown',
    }
}

let infoPromise: Promise<DiagnosticInfo> | undefined
export function loadDiagnosticInfo(): Promise<DiagnosticInfo> {
    return infoPromise ??= (async () => {
        const info = fallbackInfo()
        await Promise.all([
            Promise.resolve().then(getDetailedOSLabel).then(os => { info.os = os }, () => {}),
            isTauri ? Promise.resolve().then(() => invoke<{ model: string | null; arch: string }>('diagnostic_device_info'))
                .then(device => { info.device = device.model ? `${device.model} (${device.arch})` : device.arch }, () => {}) : undefined,
        ])
        return info
    })()
}

export const diagnosticInfo = readable(fallbackInfo(), set => {
    let active = true
    void loadDiagnosticInfo().then(info => { if (active) set(info) })
    return () => { active = false }
})

const version = `${versionData.version} (RisuAI ${appVer})`
const singleLine = (value: string) => value.replace(/[\r\n\t]+/g, ' ').trim()

export function formatErrorDetails(info: DiagnosticInfo, message: string, stack = '', notice = ''): string {
    const firstLine = stack.split('\n', 1)[0]
    const body = stack && (firstLine === message || firstLine.endsWith(`: ${message}`))
        ? stack : [message, stack].filter(Boolean).join('\n\n')
    return [
        `RisuNest Version: ${version}`,
        `OS: ${singleLine(info.os)}`,
        `Device: ${singleLine(info.device)}`,
        `User-Agent: ${singleLine(info.userAgent)}`,
        '',
        ...[notice, body].filter(Boolean),
    ].join('\n')
}

export function formatDiagnosticLog(info: DiagnosticInfo, log: string): string {
    if (!log) return ''
    return `RisuNest ${version} | OS: ${singleLine(info.os)} | Device: ${singleLine(info.device)} | UA: ${singleLine(info.userAgent)}\n\n${log}`
}
