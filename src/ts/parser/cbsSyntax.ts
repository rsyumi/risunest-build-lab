// A parser call on text without these openers has no CBS work or substitutions.
export function hasCbsSyntax(text: string): boolean {
    return /\{[{#]|<(?:user|char|bot)>/i.test(text)
}
