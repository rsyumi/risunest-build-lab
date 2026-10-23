import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import versionData from '../../version.json'
import { appVer } from './appVersion'

const native = vi.hoisted(() => ({ invoke: vi.fn(), os: 'android', version: '16' }))
vi.mock('@tauri-apps/api/core', async importOriginal => ({
    ...await importOriginal<typeof import('@tauri-apps/api/core')>(), invoke: native.invoke,
}))
vi.mock('@tauri-apps/plugin-os', () => ({ arch: () => 'aarch64', type: () => native.os, version: () => native.version }))

beforeEach(() => {
    vi.resetModules()
    Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} })
    native.invoke.mockReset()
    native.os = 'android'
    native.version = '16'
})
afterEach(() => {
    Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
    vi.resetModules()
})

describe('diagnostic reports', () => {
    it.each([
        ['Windows 11 Professional', '10.0.26100', 'Windows 11 Professional (NT 10.0.26100)'],
        ['Windows 11 Professional', null, 'Windows 11 Professional'],
    ])('reports the native Windows product name and available kernel version: %s', async (name, version, expected) => {
        native.os = 'windows'
        native.version = '10.0.26100'
        native.invoke.mockResolvedValue({ model: null, arch: 'x86_64', os: { platform: 'windows', name, version } })
        const { loadDiagnosticInfo, formatErrorDetails, formatDiagnosticLog } = await import('./diagnostics')
        const info = await loadDiagnosticInfo()
        expect(info.os).toBe(expected)
        expect(info.device).toBe('x86_64')
        expect(formatErrorDetails(info, 'synthetic failure')).toContain(`\nOS: ${expected}\n`)
        expect(formatDiagnosticLog(info, '[error] synthetic')).toContain(` | OS: ${expected} | `)
    })

    it.each(['unavailable command', 'older response'])('identifies the Windows kernel version when native product details are missing: %s', async mode => {
        native.os = 'windows'
        native.version = '10.0.26100'
        if (mode === 'unavailable command') native.invoke.mockRejectedValue(new Error('bridge unavailable'))
        else native.invoke.mockResolvedValue({ model: null, arch: 'x86_64' })
        const { loadDiagnosticInfo } = await import('./diagnostics')
        expect((await loadDiagnosticInfo()).os).toBe('Windows (NT 10.0.26100)')
    })

    it.each([
        ['Ubuntu', '24.4.0', 'Ubuntu 24.4.0'],
        ['Debian', '13.0.0', 'Debian 13.0.0'],
        ['Arch Linux', 'Rolling Release', 'Arch Linux Rolling Release'],
        ['Linux', null, 'Linux'],
    ])('reports the Linux distribution and available release: %s', async (name, version, expected) => {
        native.os = 'linux'
        native.version = '24.4.0'
        native.invoke.mockResolvedValue({ model: null, arch: 'x86_64', os: { platform: 'linux', name, version } })
        const { loadDiagnosticInfo, formatErrorDetails, formatDiagnosticLog } = await import('./diagnostics')
        const info = await loadDiagnosticInfo()
        expect(info.os).toBe(expected)
        expect(formatErrorDetails(info, 'synthetic failure')).toContain(`\nOS: ${expected}\n`)
        expect(formatDiagnosticLog(info, '[error] synthetic')).toContain(` | OS: ${expected} | `)
    })

    it.each(['unavailable command', 'older response'])('keeps the Linux fallback when native distribution details are missing: %s', async mode => {
        native.os = 'linux'
        native.version = '24.4.0'
        if (mode === 'unavailable command') native.invoke.mockRejectedValue(new Error('bridge unavailable'))
        else native.invoke.mockResolvedValue({ model: null, arch: 'x86_64' })
        const { loadDiagnosticInfo } = await import('./diagnostics')
        expect((await loadDiagnosticInfo()).os).toBe('Linux 24.4.0')
    })

    it.each([
        ['Samsung SM-S928N', 'Samsung SM-S928N (aarch64)'],
        [null, 'aarch64'],
    ])('uses an available native model or just the architecture: %s', async (model, expected) => {
        native.invoke.mockResolvedValue({ model, arch: 'aarch64', os: null })
        const { loadDiagnosticInfo, formatErrorDetails, formatDiagnosticLog } = await import('./diagnostics')
        const info = await loadDiagnosticInfo()
        expect(info.device).toBe(expected)
        expect(info.os).toBe('Android 16')
        const report = formatErrorDetails(info, 'synthetic failure')
        expect(report).toBe([
            `RisuNest Version: ${versionData.version} (RisuAI ${appVer})`,
            'OS: Android 16', `Device: ${expected}`, `User-Agent: ${navigator.userAgent}`,
            '', 'synthetic failure',
        ].join('\n'))
        expect(formatDiagnosticLog(info, '[error] synthetic')).toBe(
            `RisuNest ${versionData.version} (RisuAI ${appVer}) | OS: Android 16 | Device: ${expected} | UA: ${navigator.userAgent}\n\n[error] synthetic`,
        )
        expect(formatDiagnosticLog(info, '')).toBe('')
    })

    it('keeps reports usable when the native model query fails', async () => {
        native.invoke.mockRejectedValue(new Error('bridge unavailable'))
        const { loadDiagnosticInfo, formatErrorDetails } = await import('./diagnostics')
        const info = await loadDiagnosticInfo()
        expect(info.device).toBe('aarch64')
        const stack = 'Error: synthetic failure\n    at synthetic.ts:1:2'
        const report = formatErrorDetails(info, 'synthetic failure', stack, 'source map unavailable')
        expect(report).toContain('\nsource map unavailable\n' + stack)
        expect(report.match(/synthetic failure/g)).toHaveLength(1)
    })

    it('keeps metadata on one line without changing the log body', async () => {
        const { formatDiagnosticLog } = await import('./diagnostics')
        const text = formatDiagnosticLog({ os: 'Linux\n6', device: 'Example\tPC', userAgent: 'UA\r\n1' }, 'first\nsecond')
        expect(text.split('\n')).toEqual([
            `RisuNest ${versionData.version} (RisuAI ${appVer}) | OS: Linux 6 | Device: Example PC | UA: UA 1`,
            '', 'first', 'second',
        ])
    })
})
