import type { NativePortableRestorePreview, NativePortableSelection } from '../nativeFileJobs'
import { nativePortableDeviceSections } from './selection'

export async function selectPortableBackupExport(): Promise<NativePortableSelection> {
    return { library: true, deviceSections: [...nativePortableDeviceSections] }
}

export async function selectPortableBackupRestore(
    preview: NativePortableRestorePreview,
    _options: { firstRun?: boolean } = {},
): Promise<NativePortableSelection> {
    if (!preview.libraryIncluded || preview.repairRequired ||
        preview.deviceSections.length !== nativePortableDeviceSections.length ||
        new Set(preview.deviceSections).size !== nativePortableDeviceSections.length ||
        nativePortableDeviceSections.some((section) => !preview.deviceSections.includes(section))) {
        throw new Error('The full backup is incomplete or requires repair')
    }
    return { library: true, deviceSections: [...nativePortableDeviceSections] }
}
