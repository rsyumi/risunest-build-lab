import type { Database, botPreset } from './database.svelte'
import type { PreparedBootstrapDatabase } from './databasePreparation'
import {
    RevisionConflictError,
    type CharacterSummary,
    type DataRevision,
    type PersistentDataStore,
    type PersistentRoot,
    type PresetCatalog,
    type PresetSummary,
} from './persistentDataStore'
import { yieldToMainThread } from '../ui/yieldToUi'

const BOOTSTRAP_CATALOG_PAGE_SIZE = 200
const NEW_DATABASE_SEED: Partial<Database> = {
    streamingDisplayOptimizationMode: 'balanced',
}

export interface PersistentBootstrapDependencies {
    store: PersistentDataStore
    onPhase?(
        phase: 'storage' | 'data' | 'compatibility',
        language?: string,
    ): void | Promise<void>
    prepareDatabase(database: Database): Promise<PreparedBootstrapDatabase>
    prepareRoot?(root: PersistentRoot): Promise<PersistentRoot>
    projectScalableWorkingSet?(input: ScalableBootstrapProjection): Database
    now?(): number
}

export interface ScalableBootstrapProjection {
    root: PersistentRoot
    characters: CharacterSummary[]
    presetCatalog: PresetCatalog
    activePreset: {
        summary: PresetSummary
        value: botPreset
    } | null
}

export interface PersistentBootstrapResult {
    database: Database
    revision: DataRevision
}

/**
 * Opens the authoritative local store. A store that has never been written starts from an empty
 * database, because existing RisuAI data only enters the app through backup import.
 */
export async function bootstrapPersistentDatabase(
    dependencies: PersistentBootstrapDependencies,
): Promise<PersistentBootstrapResult> {
    await dependencies.onPhase?.('storage')
    await dependencies.store.open()
    const active = await dependencies.store.readRoot()
    await dependencies.onPhase?.('data', active.value.language)

    if (active.revision === 0) {
        const { database } = await dependencies.prepareDatabase({ ...NEW_DATABASE_SEED } as Database)
        const { revision } = await dependencies.store.replaceFromDatabase(database, 0)
        if (dependencies.projectScalableWorkingSet) {
            const {
                characters: _characters,
                botPresets: _botPresets,
                pluginCustomStorage: _pluginCustomStorage,
                pluginStorageMeta: _pluginStorageMeta,
                ...storedRoot
            } = database
            const root = await canonicalizePresetSelection(
                dependencies.store,
                revision,
                storedRoot,
            )
            const projected = await projectScalableRevision(dependencies, root, revision)
            return projected
        }
        return { database, revision }
    }

    if (!dependencies.prepareRoot || !dependencies.projectScalableWorkingSet) {
        await dependencies.onPhase?.('compatibility', active.value.language)
        const persistent = await dependencies.store.materializeDatabase(active.revision)
        const { database } = await dependencies.prepareDatabase(persistent)
        assertRevision(active.revision, (await dependencies.store.readRoot()).revision)
        return { database, revision: active.revision }
    }

    const preparedRoot = await dependencies.prepareRoot(active.value)
    const root = await canonicalizePresetSelection(
        dependencies.store,
        active.revision,
        preparedRoot,
    )
    return await projectScalableRevision(dependencies, root, active.revision)
}

async function canonicalizePresetSelection(
    store: PersistentDataStore,
    revision: DataRevision,
    root: PersistentRoot,
): Promise<PersistentRoot> {
    const catalog = await store.queryPresets()
    assertRevision(revision, catalog.revision)
    if (
        (typeof root.botPresetsId === 'number' && root.botPresetsId < 0) ||
        catalog.items.some((item) => typeof root.botPresetsId === 'string' ? item.id === root.botPresetsId : item.configuredIndex === root.botPresetsId)
    ) {
        return root
    }
    const first = [...catalog.items].sort(
        (left, right) => left.configuredIndex - right.configuredIndex,
    )[0]
    return {
        ...root,
        botPresetsId: first?.id ?? '',
    }
}

async function projectScalableRevision(
    dependencies: PersistentBootstrapDependencies,
    root: PersistentRoot,
    revision: DataRevision,
): Promise<{ database: Database; revision: DataRevision }> {
    const currentRoot = root
    const currentRevision = revision
    const characters = await queryAllCharacterSummaries(dependencies.store, revision)
    const presets = await readSelectedPreset(
        dependencies.store,
        currentRevision,
        currentRoot.botPresetsId,
    )
    return {
        revision: currentRevision,
        database: dependencies.projectScalableWorkingSet!({
        root: currentRoot,
        characters,
        presetCatalog: presets.catalog,
        activePreset: presets.active,
        }),
    }
}

async function queryAllCharacterSummaries(
    store: PersistentDataStore,
    revision: DataRevision,
): Promise<CharacterSummary[]> {
    const characters: CharacterSummary[] = []
    const trashStates = [false, true] as const
    for (const [trashIndex, trash] of trashStates.entries()) {
        let cursor: string | undefined
        do {
            const page = await store.queryCharacters({
                order: 'configured',
                trash,
                limit: BOOTSTRAP_CATALOG_PAGE_SIZE,
                cursor,
            })
            assertRevision(revision, page.revision)
            characters.push(...page.items)
            cursor = page.nextCursor
            const hasAnotherPage =
                cursor !== undefined || trashIndex < trashStates.length - 1
            if (hasAnotherPage) await yieldToMainThread()
        } while (cursor !== undefined)
    }
    return characters
}

async function readSelectedPreset(
    store: PersistentDataStore,
    revision: DataRevision,
    configuredIndex: number | string,
): Promise<{
    catalog: PresetCatalog
    active: ScalableBootstrapProjection['activePreset']
}> {
    const catalog = await store.queryPresets()
    assertRevision(revision, catalog.revision)
    const summary = catalog.items.find((item) => typeof configuredIndex === 'string' ? item.id === configuredIndex : item.configuredIndex === configuredIndex)
    if (!summary) return { catalog, active: null }
    const preset = await store.readPreset(summary.id)
    if (!preset) throw new Error(`Preset ${summary.id} was not found`)
    assertRevision(revision, preset.revision)
    return {
        catalog,
        active: { summary, value: preset.value },
    }
}

function assertRevision(expected: DataRevision, actual: DataRevision): void {
    if (actual !== expected) throw new RevisionConflictError(expected, actual)
}
