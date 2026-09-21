import { isRealmPath, isRealmUrl } from '../../scripts/realmBlocklist.mjs'

export function classifyTestRequest(input: unknown, origins: ReadonlySet<string>): string | null {
    const value = typeof input === 'object' && input !== null && 'url' in input ? input.url : input
    if (isRealmUrl(value) || isRealmPath(value)) return 'RisuRealm access is forbidden'
    let url: URL
    try { url = new URL(String(value)) }
    catch { return 'Unplanned relative or invalid request' }
    if (url.protocol === 'data:' || url.protocol === 'blob:') return null
    if ((url.protocol === 'http:' || url.protocol === 'https:') && origins.has(url.origin)) return null
    return 'Unexpected test network request'
}

export function createTestNetworkPolicy() {
    const origins = new Set<string>()
    const rejected: string[] = []
    return {
        allowLoopbackOrigin(origin: string) {
            const url = new URL(origin)
            if (!['http:', 'https:'].includes(url.protocol)
                || !['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname)
                || url.origin !== origin) throw new Error('Expected an exact loopback origin')
            origins.add(origin)
            return () => { origins.delete(origin) }
        },
        check(input: unknown) {
            const reason = classifyTestRequest(input, origins)
            if (!reason) return
            rejected.push(reason)
            throw new Error(reason)
        },
        finish() {
            const failures = rejected.splice(0)
            origins.clear()
            if (failures.length) throw new Error(`Blocked test requests (${failures.length}): ${failures.join('; ')}`)
        },
    }
}

export const testNetwork = createTestNetworkPolicy()
export const allowTestLoopbackOrigin = testNetwork.allowLoopbackOrigin
