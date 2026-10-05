import type { MCPTool, RPCToolCallContent } from '../process/mcp/mcplib'
import { isPlainObject, throwIfAborted } from './pluginQueryInput'

export interface HostToolSource {
    serverInfo?: { serverInfo?: { name?: string } }
    checkHandshake(): Promise<unknown>
    getToolList(): Promise<MCPTool[]>
    callTool(name: string, args: unknown): Promise<RPCToolCallContent[]>
}

export interface HostTool {
    source: string
    sourceName?: string
    name: string
    description?: string
    inputSchema: object
    annotations?: object
}

export interface HostToolList {
    /** Binds calls to the conversation selected when the list was taken. */
    scope: string
    tools: HostTool[]
}

export interface HostToolCallRequest {
    scope: string
    source: string
    name: string
    arguments: unknown
}

export interface HostToolBridgeDependencies {
    /** The host's own sources, initialized as for a generation: the selected chat's module MCPs and the call-only ones. */
    loadHostSources(): Promise<ReadonlyMap<string, HostToolSource>>
    /** Built-in sources this platform supports. */
    builtInSourceIds(): readonly string[]
    createBuiltInSource(id: string): Promise<HostToolSource | null>
    pluginSources(): ReadonlyMap<string, HostToolSource>
    pluginSourceOwner(source: string): string | undefined
    captureSelectedConversation(): { characterId: string; conversationId: string } | null
}

export interface HostToolAccess {
    listTools(): Promise<HostToolList>
    callTool(request: HostToolCallRequest, signal?: AbortSignal): Promise<RPCToolCallContent[]>
}

function requiredString(value: unknown, name: string): string {
    if (typeof value !== 'string' || !value) throw new TypeError(`${name} must be a non-empty string`)
    return value
}

export function normalizeHostToolCallInput(input: unknown): HostToolCallRequest {
    if (!isPlainObject(input)) throw new TypeError('Host tool call input must be an object')
    return {
        scope: requiredString(input.scope, 'scope'),
        source: requiredString(input.source, 'source'),
        name: requiredString(input.name, 'name'),
        arguments: input.arguments,
    }
}

export function createHostToolBridge(dependencies: HostToolBridgeDependencies) {
    // Built-in clients the module set does not hold yet. They are created here instead of in
    // the generation's registry, which drops them, and they shake hands on first call because
    // some ask the user for a folder or a key.
    const builtIns = new Map<string, { source: Promise<HostToolSource | null>; handshake?: Promise<unknown> }>()

    const scopeOf = () => {
        const selected = dependencies.captureSelectedConversation()
        return JSON.stringify(selected ? [selected.characterId, selected.conversationId] : null)
    }

    async function resolveSources(caller: string) {
        const sources = new Map<string, { source: HostToolSource; builtIn: boolean }>()
        for (const [id, source] of await dependencies.loadHostSources()) sources.set(id, { source, builtIn: false })
        for (const id of dependencies.builtInSourceIds()) {
            if (sources.has(id)) continue
            let entry = builtIns.get(id)
            if (!entry) {
                entry = { source: dependencies.createBuiltInSource(id) }
                builtIns.set(id, entry)
            }
            const source = await entry.source.catch((error) => {
                builtIns.delete(id)
                console.warn(`Host tool source ${id} could not be created`, error)
                return null
            })
            if (source) sources.set(id, { source, builtIn: true })
        }
        for (const [id, source] of dependencies.pluginSources()) {
            if (!sources.has(id)) sources.set(id, { source, builtIn: false })
        }
        for (const id of sources.keys()) {
            if (id.startsWith('plugin:') && dependencies.pluginSourceOwner(id) === caller) sources.delete(id)
        }
        return sources
    }

    function handshake(id: string, source: HostToolSource): Promise<unknown> {
        const entry = builtIns.get(id)
        if (!entry) return source.checkHandshake()
        entry.handshake ??= source.checkHandshake().catch((error) => {
            entry.handshake = undefined
            throw error
        })
        return entry.handshake
    }

    async function listTools(caller: string): Promise<HostToolList> {
        const scope = scopeOf()
        const tools: HostTool[] = []
        for (const [id, { source }] of await resolveSources(caller)) {
            let listed: MCPTool[]
            try {
                listed = await source.getToolList()
            } catch (error) {
                console.warn(`Host tool source ${id} could not list its tools`, error)
                continue
            }
            const sourceName = source.serverInfo?.serverInfo?.name
            for (const tool of listed) {
                tools.push({
                    source: id,
                    ...(sourceName ? { sourceName } : {}),
                    name: tool.name,
                    ...(tool.description ? { description: tool.description } : {}),
                    inputSchema: tool.inputSchema ?? {},
                    ...(tool.annotations ? { annotations: tool.annotations } : {}),
                })
            }
        }
        return { scope, tools }
    }

    function assertScope(request: HostToolCallRequest): void {
        if (request.scope !== scopeOf()) {
            throw new Error('The selected conversation changed after the host tools were listed')
        }
    }

    async function callTool(caller: string, request: HostToolCallRequest, signal?: AbortSignal): Promise<RPCToolCallContent[]> {
        throwIfAborted(signal)
        assertScope(request)
        const entry = (await resolveSources(caller)).get(request.source)
        if (!entry) throw new Error(`Host tool source ${request.source} is not available`)
        if (entry.builtIn) await handshake(request.source, entry.source)
        if (!(await entry.source.getToolList()).some((tool) => tool.name === request.name)) {
            throw new Error(`Host tool source ${request.source} has no tool ${request.name}`)
        }
        throwIfAborted(signal)
        // The selection can change while a built-in asks the user for a folder or a key.
        assertScope(request)
        const result = await entry.source.callTool(request.name, request.arguments)
        throwIfAborted(signal)
        return result
    }

    return {
        /** The access one plugin uses; its own MCPs are left out. */
        forPlugin(caller: string): HostToolAccess {
            return {
                listTools: () => listTools(caller),
                callTool: (request, signal) => callTool(caller, request, signal),
            }
        },
    }
}
