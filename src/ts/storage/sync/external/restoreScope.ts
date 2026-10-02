import type { ExternalHistoryItem, ExternalRestoreArea, ExternalRestoreSection } from './types'

const SECTIONS: ExternalRestoreSection[] = ['hypa', 'local-plugins', 'local-settings']
export const FULL_EXTERNAL_RESTORE_AREAS: readonly ExternalRestoreArea[] = [
    'library', 'referencedAssets', ...SECTIONS,
]

export function externalRestorableSections(
    item: Pick<ExternalHistoryItem, 'includedSections' | 'sameDevice'>,
): ExternalRestoreSection[] {
    if (SECTIONS.some(section => !item.includedSections.includes(section))) {
        throw new Error('The backup does not contain the complete restore scope')
    }
    return [...SECTIONS]
}

export function externalRestoreAreas(
    item: Pick<ExternalHistoryItem, 'includedSections' | 'sameDevice'>,
    _chosen: readonly ExternalRestoreSection[],
): ExternalRestoreArea[] {
    externalRestorableSections(item)
    return [...FULL_EXTERNAL_RESTORE_AREAS]
}
