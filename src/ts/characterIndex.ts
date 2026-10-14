export function indexCharactersById(characters: readonly { chaId: string }[]) {
    let i=0;
    let result:{[key:string]:number} = {}
    for(const char of characters){
        result[char.chaId] = i
        i += 1
    }
    return result
}
