import { language } from 'src/lang'

export class StdioModuleImportError extends Error {
    constructor() {
        super(language.mcpStdioModuleImportBlocked)
        this.name = 'StdioModuleImportError'
    }
}

export function assertModuleMCPImportAllowed(module: { mcp?: { url?: unknown } }) {
    if (typeof module.mcp?.url === 'string' && module.mcp.url.startsWith('stdio:')) {
        throw new StdioModuleImportError()
    }
}
