/** Library lookups by character ID that check the selected index first, since scripts usually run for the selected character. */

interface IdentifiedCharacter {
    chaId: string
}

export function findCharacterIndexById(
    characters: readonly IdentifiedCharacter[],
    characterId: string,
    selectedIndex: number,
): number {
    if (characters[selectedIndex]?.chaId === characterId) return selectedIndex
    return characters.findIndex((character) => character.chaId === characterId)
}

export function findCharacterById<T extends IdentifiedCharacter>(
    characters: readonly T[],
    characterId: string,
    selectedIndex: number,
): T | undefined {
    const selected = characters[selectedIndex]
    if (selected?.chaId === characterId) return selected
    return characters.find((character) => character.chaId === characterId)
}
