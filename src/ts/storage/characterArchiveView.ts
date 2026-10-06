import type { character } from './database.svelte'
import { getCatalogCharacterMetadata, isArchivedCharacter } from './workingSetCatalog'

export function characterIsArchived(value: character): boolean {
    return isArchivedCharacter(value)
}

export function archivedConversationCount(value: character): number {
    return getCatalogCharacterMetadata(value)?.conversationCount ?? 0
}

export function archivedAt(value: character): number | undefined {
    return getCatalogCharacterMetadata(value)?.archivedAt
}



export function formatArchivedAt(value: number | undefined): string {
    if (value === undefined) return ''
    return new Date(value).toLocaleDateString()
}

export function countArchivedCharacters(
    characters: readonly (character)[],
): number {
    let archived = 0
    for (const character of characters) {
        if (isArchivedCharacter(character)) archived += 1
    }
    return archived
}
