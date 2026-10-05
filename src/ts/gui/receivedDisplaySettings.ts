import { getStartupExclusions } from '../storage/deviceSettings'
import { isStartupExcluded } from '../storage/recoveryMode.svelte'

const colorFields: ReadonlySet<string> = new Set(['colorScheme', 'colorSchemeName', 'customColorScheme'])
const textFields: ReadonlySet<string> = new Set(['textTheme', 'customTextTheme', 'customCSS', 'font', 'customFont'])

/** Applies the display settings a received change set in the working set, the way startup applies them. */
export async function applyReceivedDisplaySettings(fields: ReadonlySet<string>): Promise<void> {
    const colors = [...fields].some((field) => colorFields.has(field))
    // The standard text colors follow the color scheme's light or dark type.
    const text = (colors || [...fields].some((field) => textFields.has(field))) &&
        !isStartupExcluded('theme', getStartupExclusions())
    if (!colors && !text) return
    const { updateColorScheme, updateTextThemeAndCSS } = await import('./colorscheme')
    if (colors) updateColorScheme()
    if (text) updateTextThemeAndCSS()
}
