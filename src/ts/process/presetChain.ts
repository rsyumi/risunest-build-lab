export interface PresetChainState {
    presetChain?: string
    botPresets: Array<{ name?: string }>
}

export async function activatePresetChain(
    database: PresetChainState,
    setOverride: (index: number | null) => Promise<void>,
    random: () => number,
    onMissing: (name: string) => void,
): Promise<void> {
    if (!database.presetChain) {
        await setOverride(null)
        return
    }
    const names = database.presetChain.split(',').map((name) => name.trim())
    const selectedName = names[Math.floor(random() * names.length)]
    const index = database.botPresets.findIndex((preset) => preset.name === selectedName)
    if (index < 0) {
        await setOverride(null)
        onMissing(selectedName)
        return
    }
    await setOverride(index)
}

export async function activatePresetChainForRequest(
    database: PresetChainState,
    setOverride: (index: number | null) => Promise<void>,
    random: () => number,
    onMissing: (name: string) => void,
): Promise<void> {
    await activatePresetChain(database, setOverride, random, onMissing)
}
