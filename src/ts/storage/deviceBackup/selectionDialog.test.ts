import { describe, expect, it } from 'vitest'
import { selectPortableBackupExport, selectPortableBackupRestore } from './selectionDialog'
import type { NativePortableRestorePreview } from '../nativeFileJobs'

const full = { library: true, deviceSections: ['hypa', 'local-plugins', 'local-settings'] }
function preview(overrides: Partial<NativePortableRestorePreview> = {}): NativePortableRestorePreview {
    return { libraryIncluded: true, repairRequired: false, deviceSections: ['hypa', 'local-plugins', 'local-settings'], ...overrides }
}
describe('fixed full portable backup scope', () => {
    it('exports every required section without exclusion controls', async () => {
        await expect(selectPortableBackupExport()).resolves.toEqual(full)
        expect(document.querySelector('dialog')).toBeNull()
    })
    it('restores every present required section, including empty sections', async () => {
        await expect(selectPortableBackupRestore(preview())).resolves.toEqual(full)
        await expect(selectPortableBackupRestore(preview(), { firstRun: true })).resolves.toEqual(full)
    })
    it.each(['hypa', 'local-plugins', 'local-settings'])('rejects missing %s before activation', async section => {
        await expect(selectPortableBackupRestore(preview({ deviceSections: preview().deviceSections.filter(value => value !== section) }))).rejects.toThrow('incomplete')
    })
    it('rejects a missing library and a damaged archive', async () => {
        await expect(selectPortableBackupRestore(preview({ libraryIncluded: false }))).rejects.toThrow('incomplete')
        await expect(selectPortableBackupRestore(preview({ repairRequired: true }))).rejects.toThrow('repair')
    })
    it('rejects duplicate and foreign section identifiers', async () => {
        await expect(selectPortableBackupRestore(preview({ deviceSections: ['hypa', 'hypa', 'local-plugins'] }))).rejects.toThrow('incomplete')
        await expect(selectPortableBackupRestore(preview({ deviceSections: ['hypa', 'local-plugins', 'browser-settings' as never] }))).rejects.toThrow('incomplete')
    })
})
