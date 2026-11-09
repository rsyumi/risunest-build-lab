import { createServer } from 'node:http'

const CONTROL_PREFIX = '/__fixture/'
const DEFAULT_REPLY = { status: 200, headers: {}, body: undefined, content: 'Hello.' }
const CONTROL_HEADERS = {
    'content-type': 'application/json',
    'access-control-allow-origin': '*',
    'access-control-allow-methods': 'GET, POST',
    'access-control-allow-headers': 'content-type',
}

/**
 * A synthetic Ollama endpoint on the loopback interface. It answers buffered
 * `/api/chat` requests only, counts what reaches it, and can keep a reply back
 * until the test releases it. It never contacts another host.
 *
 * With `control`, a page that cannot reach the test process drives it over
 * `/__fixture/` (`state`, `hold`, `hold-body`, `release`, `reply`, `reset`), which is not counted.
 */
export async function startOllamaFixture({ host = '127.0.0.1', control = false, corsOrigin } = {}) {
    if (corsOrigin !== undefined) {
        const parsed = new URL(corsOrigin)
        if (!['http:', 'https:'].includes(parsed.protocol) || parsed.origin !== corsOrigin) {
            throw new Error('Fixture CORS requires one exact HTTP origin')
        }
    }
    const requests = []
    const waiters = []
    const held = []
    const heldBodies = new Map()
    const bodyWaiters = []
    let reply = { ...DEFAULT_REPLY }
    let holding = false
    let aborted = 0
    let bodyHold = null
    let bodyStarted = 0

    const notify = () => {
        for (const waiter of [...waiters]) {
            if (requests.length >= waiter.count) {
                waiters.splice(waiters.indexOf(waiter), 1)
                waiter.resolve()
            }
        }
    }

    const preparedReply = () => {
        const { status, headers, body, content } = reply
        const payload = body === undefined
            ? JSON.stringify({
                model: 'fixture',
                created_at: '2026-01-01T00:00:00Z',
                message: { role: 'assistant', content },
                done: true,
            })
            : typeof body === 'string' || body instanceof Uint8Array ? body : JSON.stringify(body)
        return { status, headers: { 'content-type': 'application/json', ...headers }, payload: Buffer.from(payload) }
    }
    const send = (response) => {
        const prepared = bodyHold ?? preparedReply()
        response.writeHead(prepared.status, prepared.headers)
        if (!bodyHold) { response.end(prepared.payload); return }
        heldBodies.set(response, prepared.remainder)
        response.write(prepared.prefix, error => {
            if (error || response.destroyed) return
            bodyStarted += 1
            for (const waiter of [...bodyWaiters]) {
                if (bodyStarted >= waiter.count) {
                    bodyWaiters.splice(bodyWaiters.indexOf(waiter), 1)
                    waiter.resolve()
                }
            }
        })
    }
    const clearBodyWaiters = reason => {
        for (const waiter of bodyWaiters.splice(0)) waiter.reject(new Error(reason))
    }

    const state = () => ({ requestCount: requests.length, heldCount: held.length + heldBodies.size,
        abortedCount: aborted, bodyStartedCount: bodyStarted })
    const hold = () => { holding = true; bodyHold = null }
    const holdBody = (prefixBytes = 1) => {
        const prepared = preparedReply()
        if (!Number.isSafeInteger(prefixBytes) || prefixBytes <= 0 || prefixBytes >= prepared.payload.length) {
            throw new Error('Body hold requires a nonempty prefix and remainder')
        }
        holding = false
        bodyHold = { ...prepared, prefix: prepared.payload.subarray(0, prefixBytes), remainder: prepared.payload.subarray(prefixBytes) }
    }
    const release = () => {
        holding = false
        bodyHold = null
        for (const response of held.splice(0)) send(response)
        for (const [response, remainder] of heldBodies) {
            heldBodies.delete(response)
            if (!response.destroyed && !response.writableEnded) response.end(remainder)
        }
    }
    const controlled = (request, response, json) => {
        const action = request.url.slice(CONTROL_PREFIX.length)
        const answer = (status, body) => {
            response.writeHead(status, CONTROL_HEADERS)
            response.end(body === undefined ? undefined : JSON.stringify(body))
        }
        if (request.method === 'OPTIONS') return answer(204)
        if (request.method === 'GET' && action === 'state') return answer(200, state())
        if (request.method !== 'POST') return answer(404, { error: 'not found' })
        if (action === 'hold') hold()
        else if (action === 'hold-body') {
            try { holdBody(json?.prefixBytes) }
            catch (error) { return answer(400, { error: error.message }) }
        }
        else if (action === 'release') release()
        else if (action === 'reply' && json && typeof json === 'object') reply = { ...reply, ...json }
        else if (action === 'reset') {
            // A held reply would count as dropped after the reset, so the page releases it first.
            if (held.length || heldBodies.size) return answer(409, { error: 'replies are held' })
            requests.length = 0
            aborted = 0
            bodyStarted = 0
            bodyHold = null
            clearBodyWaiters('Fixture reset')
            holding = false
            reply = { ...DEFAULT_REPLY }
        } else return answer(404, { error: 'not found' })
        answer(200, state())
    }

    const server = createServer((request, response) => {
        if (corsOrigin !== undefined && request.url === '/api/chat') {
            const origin = request.headers.origin
            const reject = () => {
                request.resume()
                response.writeHead(403, { 'content-type': 'application/json' })
                response.end(JSON.stringify({ error: 'fixture origin or preflight refused' }))
            }
            if (request.method === 'OPTIONS') {
                const headers = String(request.headers['access-control-request-headers'] ?? '')
                    .toLowerCase().split(',').map(value => value.trim()).filter(Boolean)
                if (origin !== corsOrigin || request.headers['access-control-request-method'] !== 'POST'
                    || headers.length > 3 || headers.some(value => !['content-type', 'accept', 'user-agent'].includes(value))) return reject()
                request.resume()
                response.writeHead(204, {
                    'access-control-allow-origin': corsOrigin,
                    'access-control-allow-methods': 'POST',
                    'access-control-allow-headers': headers.join(', '),
                    vary: 'Origin',
                })
                response.end()
                return
            }
            if (origin !== undefined && origin !== corsOrigin) return reject()
            if (request.method !== 'POST') return reject()
            if (origin === corsOrigin) {
                response.setHeader('access-control-allow-origin', corsOrigin)
                response.setHeader('vary', 'Origin')
            }
        }
        const chunks = []
        request.on('data', (chunk) => chunks.push(chunk))
        request.on('end', () => {
            const text = Buffer.concat(chunks).toString('utf8')
            let json = null
            try { json = JSON.parse(text) } catch { /* the raw text stays available */ }
            if (control && request.url.startsWith(CONTROL_PREFIX)) return controlled(request, response, json)
            requests.push({ method: request.method, url: request.url, headers: request.headers, text, json })
            notify()
            if (request.method !== 'POST' || request.url !== '/api/chat') {
                response.writeHead(404, { 'content-type': 'application/json' })
                response.end(JSON.stringify({ error: 'not found' }))
                return
            }
            if (!holding) return send(response)
            held.push(response)
        })
        response.on('error', () => {})
        response.on('close', () => {
            const index = held.indexOf(response)
            if (index >= 0) held.splice(index, 1)
            heldBodies.delete(response)
            if (!response.writableEnded) aborted += 1
        })
    })

    await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(0, host, resolve)
    })
    const { port } = server.address()

    return {
        origin: `http://${host}:${port}`,
        port,
        /** Every request received so far, in arrival order. */
        requests,
        get requestCount() { return requests.length },
        /** Requests whose reply is held back right now. */
        get heldCount() { return held.length + heldBodies.size },
        /** Requests the client dropped before the reply finished. */
        get abortedCount() { return aborted },
        /** Replies whose nonempty body prefix has been written to the socket. */
        get bodyStartedCount() { return bodyStarted },
        /** Sets what buffered `/api/chat` replies carry: status, headers, a body (object, string or bytes) or the assistant text. */
        setReply(next) { reply = { ...reply, ...next } },
        /** Replies to later requests wait until `release`. */
        hold,
        /** Snapshots the configured reply and sends only its first `prefixBytes` bytes until release. */
        holdBody,
        /** Sends every held reply and stops holding. */
        release,
        /** Resolves once at least `count` requests have arrived. */
        async waitForRequests(count = 1, timeoutMs = 10_000) {
            if (requests.length >= count) return
            await new Promise((resolve, reject) => {
                const timer = setTimeout(() => {
                    waiters.splice(waiters.indexOf(waiter), 1)
                    reject(new Error(`Expected ${count} fixture requests, saw ${requests.length}`))
                }, timeoutMs)
                const waiter = { count, resolve: () => { clearTimeout(timer); resolve() } }
                waiters.push(waiter)
            })
        },
        async waitForBodyStart(count = 1, timeoutMs = 10_000) {
            if (bodyStarted >= count) return
            await new Promise((resolve, reject) => {
                const settle = callback => value => { clearTimeout(timer); callback(value) }
                const waiter = { count, resolve: settle(resolve), reject: settle(reject) }
                const timer = setTimeout(() => {
                    bodyWaiters.splice(bodyWaiters.indexOf(waiter), 1)
                    waiter.reject(new Error(`Expected ${count} started fixture bodies, saw ${bodyStarted}`))
                }, timeoutMs)
                bodyWaiters.push(waiter)
            })
        },
        async close() {
            bodyHold = null
            clearBodyWaiters('Fixture closed')
            for (const response of held.splice(0)) response.destroy()
            for (const response of heldBodies.keys()) response.destroy()
            heldBodies.clear()
            server.closeAllConnections()
            await new Promise((resolve) => server.close(resolve))
        },
    }
}
