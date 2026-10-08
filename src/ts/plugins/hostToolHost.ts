import { registeredCustomPluginMCPs, registerMCPModule, unregisterMCPModule } from '../process/mcp/pluginmcp'
import { captureSelectedConversationTarget } from '../storage/persistentDataRuntime.svelte'
import { createHostToolBridge, type HostToolSource } from './hostToolBridge'

const pluginSourceOwners = new WeakMap<object, string>()
const builtInSources = ['internal:risuai', 'internal:aiaccess', 'internal:googlesearch', 'internal:graphmem', 'internal:dice']

/** `registerMCP` for one plugin; the bridge leaves a plugin's own MCPs out of its tools. */
export async function registerOwnedPluginMCP(owner: string, ...args: Parameters<typeof registerMCPModule>): Promise<() => void> {
    const client = await registerMCPModule(...args)
    const id = args[0].identifier
    pluginSourceOwners.set(client, owner)
    return () => {
        if (registeredCustomPluginMCPs.get(id) !== client) return
        void unregisterMCPModule(id)
        pluginSourceOwners.delete(client)
    }
}

async function createBuiltInSource(id: string): Promise<HostToolSource | null> {
    switch (id) {
        case 'internal:fs': return new (await import('../process/mcp/filesystemclient')).FileSystemClient()
        case 'internal:risuai': return new (await import('../process/mcp/risuaccess')).RisuAccessClient()
        case 'internal:aiaccess': return new (await import('../process/mcp/aiaccess')).AIAccessClient()
        case 'internal:googlesearch': return new (await import('../process/mcp/googlesearchclient')).GoogleSearchClient()
        case 'internal:graphmem': return new (await import('../process/mcp/graphmem')).GraphMemClient()
        case 'internal:dice': return new (await import('../process/mcp/dice')).DiceClient()
        default: return null
    }
}

export const hostToolBridge = createHostToolBridge({
    async loadHostSources() {
        const { MCPs, callOnlyMCPs, initializeMCPs } = await import('../process/mcp/mcp')
        await initializeMCPs()
        return new Map(Object.entries({ ...MCPs, ...callOnlyMCPs }))
    },
    builtInSourceIds: () => 'showDirectoryPicker' in window ? ['internal:fs', ...builtInSources] : builtInSources,
    createBuiltInSource,
    pluginSources: () => registeredCustomPluginMCPs,
    pluginSourceOwner: (id) => {
        const client = registeredCustomPluginMCPs.get(id)
        return client ? pluginSourceOwners.get(client) : undefined
    },
    captureSelectedConversation: () => {
        const selected = captureSelectedConversationTarget()
        return selected ? { characterId: selected.characterId, conversationId: selected.conversationId } : null
    },
})
