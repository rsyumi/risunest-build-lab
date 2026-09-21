import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const backupSource = readFileSync('src/lib/Setting/Pages/RisuNestBackupRestore.svelte', 'utf8')
const performanceSource = readFileSync('src/lib/Setting/Pages/RisuNestPerformanceSettings.svelte', 'utf8')
const storageSource = readFileSync('src/lib/Setting/Pages/RisuNestStorageDashboard.svelte', 'utf8')
const androidSource = readFileSync('src/lib/Setting/Pages/RisuNestAndroidPlatform.svelte', 'utf8')
const logSource = readFileSync('src/lib/Setting/Pages/RisuNestLogViewer.svelte', 'utf8')
const serverSyncSource = readFileSync('src/lib/Setting/ServerSync/ServerSyncConnection.svelte', 'utf8')
const dataHealthSource = readFileSync('src/lib/Setting/Pages/RisuNestDataHealth.svelte', 'utf8')
const updateSource = readFileSync('src/lib/Setting/Pages/RisuNestUpdateSettings.svelte', 'utf8')
const appImageSource = readFileSync('src/lib/Setting/Pages/RisuNestAppImage.svelte', 'utf8')
const pluginDataSource = readFileSync('src/lib/Setting/RisuNest/PluginDataManager.svelte', 'utf8')
const portableSource = readFileSync('src/lib/Setting/PortableBackupSelection.svelte', 'utf8')
const assignDialogSource = readFileSync('src/lib/Setting/RisuNest/PluginValueAssignDialog.svelte', 'utf8')
const segmentedSource = readFileSync('src/lib/Setting/RisuNest/SegmentedButtons.svelte', 'utf8')
const groupSource = readFileSync('src/lib/Setting/RisuNest/SettingGroup.svelte', 'utf8')
const rowSource = readFileSync('src/lib/Setting/RisuNest/SettingRow.svelte', 'utf8')

describe('RisuNest settings theme and layout conventions', () => {
    it('uses theme tokens rather than fixed foreground and error colors', () => {
        expect(segmentedSource).not.toContain('text-white')
        expect(storageSource).not.toContain('text-red-500')
    })

    it('keeps section containers under the shared setting group', () => {
        for (const source of [
            performanceSource, storageSource, backupSource,
            androidSource, logSource, serverSyncSource,
        ]) {
            expect(source).toContain('<SettingGroup')
        }
        expect(groupSource).toContain('@container')
    })

    it('uses container rather than viewport breakpoints for setting rows', () => {
        expect(rowSource).toMatch(/@(sm|md|lg|xl):/)
        expect(rowSource).not.toMatch(/\b(sm|md|lg):/)
        // The storage dashboard skeleton used to size itself against the viewport.
        expect(storageSource).not.toMatch(/(?<![@\w-])(sm|md|lg):/)
    })

    it('builds every choice from the shared RisuNest controls', () => {
        for (const source of [dataHealthSource, pluginDataSource, portableSource, assignDialogSource]) {
            expect(source).toContain('SettingToggle')
            expect(source).not.toContain('<Check ')
            expect(source).not.toMatch(/<input\b/)
        }
        for (const source of [updateSource, appImageSource, portableSource]) {
            expect(source).toContain('SettingButton')
            expect(source).not.toMatch(/import Button from/)
        }
    })

    it('keeps one overlay tone behind the RisuNest dialogs', () => {
        for (const source of [pluginDataSource, assignDialogSource]) {
            expect(source).toContain('bg-black/60')
            expect(source).not.toContain('bg-black/65')
        }
    })

    it('never prints a raw blocked-reason token on the storage page', () => {
        expect(storageSource).toContain('describeBlockedReason')
        expect(storageSource).not.toMatch(/\{backup\.blockedReason\}/)
        expect(storageSource).not.toMatch(/\{view\.tempUsage\.blockedReason\}/)
    })
})
