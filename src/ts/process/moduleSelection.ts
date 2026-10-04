import type { RisuModule } from './modules'

export interface ModuleSelectionSource {
    modules?: RisuModule[]
    enabledModules?: string[]
    moduleIntergration?: string
}

function deduplicateModuleById(modules: RisuModule[]) {
    let ids: string[] = []
    let newModules: RisuModule[] = []
    for (let i = 0; i < modules.length; i++) {
        if (ids.includes(modules[i].id)) {
            continue
        }
        ids.push(modules[i].id)
        newModules.push(modules[i])
    }
    return newModules
}

/** The modules a prompt for one conversation uses, from explicit inputs instead of the selected chat. */
export function selectConversationModules(
    database: ModuleSelectionSource,
    chatModules: string[] | undefined,
    characterModules: string[] | undefined,
    boundPersona: { embeddedModule?: RisuModule } | null | undefined,
): RisuModule[] {
    let ids = database.enabledModules ?? []
    ids = ids.concat(chatModules ?? [])
    if (characterModules) {
        ids = ids.concat(characterModules)
    }
    if (database.moduleIntergration) {
        const intList = database.moduleIntergration.split(',').map((s) => s.trim())
        ids = ids.concat(intList)
    }
    const idSet = new Set(ids)
    const modules = deduplicateModuleById((database.modules ?? []).filter(m =>
        idSet.has(m.id) || (m.namespace && idSet.has(m.namespace))
    ))
    if (boundPersona?.embeddedModule) {
        modules.push(boundPersona.embeddedModule)
    }
    return deduplicateModuleById(modules)
}
