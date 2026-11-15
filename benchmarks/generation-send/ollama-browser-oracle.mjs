import { Buffer } from 'node:buffer'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:http'
import { createRequire } from 'node:module'
import { basename, dirname, isAbsolute, relative, resolve, sep } from 'node:path'
import { setImmediate as immediate } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import * as zlib from 'node:zlib'
import { isRealmPath, isRealmUrl } from '../../scripts/realmBlocklist.mjs'

function deferred() {
    /** @type {(value?: unknown) => void} */
    let fulfill
    const promise = new Promise(resolve => { fulfill = resolve })
    return { promise, resolve: fulfill }
}

export function encodeJsText(value) {
    for (let index = 0; index < value.length; index++) {
        const unit = value.charCodeAt(index)
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = value.charCodeAt(index + 1)
            if (next >= 0xdc00 && next <= 0xdfff) { index++; continue }
        } else if (unit < 0xdc00 || unit > 0xdfff) continue
        return { utf16: Array.from({ length: value.length }, (_, offset) => value.charCodeAt(offset)) }
    }
    return value
}

export function encodeDiagnosticValue(value) {
    if (typeof value === 'string') return encodeJsText(value)
    if (typeof value === 'number' && (!Number.isFinite(value) || Object.is(value, -0))) return { number: String(Object.is(value, -0) ? '-0' : value) }
    if (value === undefined) return { undefined: true }
    if (Array.isArray(value)) return value.map(encodeDiagnosticValue)
    if (value !== null && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encodeDiagnosticValue(item)]))
    return value
}

async function streamedZstd(input, windowLog, signal) {
    signal.throwIfAborted()
    const params = { [zlib.constants.ZSTD_c_windowLog]: windowLog, [zlib.constants.ZSTD_c_contentSizeFlag]: 0 }
    const encoder = zlib.createZstdCompress({ params })
    const closed = new Promise(resolve => encoder.once('close', resolve))
    const operations = new Set()
    let failed = false
    let failure
    function stop(error) {
        if (failed) return
        failed = true
        failure = error
        for (const finish of [...operations]) finish(error)
        encoder.destroy(error)
    }
    const onAbort = () => stop(signal.reason)
    encoder.on('error', stop)
    signal.addEventListener('abort', onAbort, { once: true })
    function callbackOperation(start) {
        if (failed) return Promise.reject(failure)
        return new Promise((resolve, reject) => {
            const finish = error => {
                if (!operations.delete(finish)) return
                error ? reject(error) : resolve()
            }
            operations.add(finish)
            try { start(finish) } catch (error) { finish(error) }
        })
    }
    const chunks = []
    let total = 0
    const collected = (async () => {
        for await (const chunk of encoder) {
            total += chunk.length
            if (total > 256 * 1024) throw new Error('Synthetic encoded body bound exceeded')
            chunks.push(chunk)
        }
    })()
    void collected.catch(stop)
    try {
        if (signal.aborted) onAbort()
        await callbackOperation(finish => encoder.write(input.subarray(0, 1), finish))
        await callbackOperation(finish => encoder.flush(zlib.constants.ZSTD_e_flush, finish))
        encoder.end(input.subarray(1))
        await collected
        signal.throwIfAborted()
        return Buffer.concat(chunks)
    } finally {
        signal.removeEventListener('abort', onAbort)
        encoder.destroy()
        await collected.catch(() => {})
        await closed
        encoder.removeListener('error', stop)
    }
}

async function responseBytes(entry, signal) {
    signal.throwIfAborted()
    const raw = Buffer.concat(entry.body.map(part => part.hex === undefined ? Buffer.from(part.utf8, 'utf8') : Buffer.from(part.hex, 'hex')))
    if (raw.length > 64 * 1024) throw new Error('Synthetic response fixture exceeds its bound')
    const codings = entry.encodings ?? (entry.encoding ? [entry.encoding] : [])
    if (codings.length > 4) throw new Error('Synthetic codec layer bound exceeded')
    let encoded = raw
    for (const coding of codings) {
        signal.throwIfAborted()
        if (coding === 'gzip') {
            if (entry.gzipMemberSplitAt !== undefined) {
                const split = entry.gzipMemberSplitAt
                if (!Number.isInteger(split) || split <= 0 || split >= encoded.length) throw new Error('Invalid synthetic gzip member split')
                encoded = Buffer.concat([zlib.gzipSync(encoded.subarray(0, split)), zlib.gzipSync(encoded.subarray(split))])
            } else encoded = zlib.gzipSync(encoded)
        } else if (coding === 'deflate') encoded = zlib.deflateSync(encoded)
        else if (coding === 'deflate-raw') encoded = zlib.deflateRawSync(encoded)
        else if (coding === 'br') encoded = zlib.brotliCompressSync(encoded)
        else if (coding === 'zstd') {
            if (typeof zlib.zstdCompressSync !== 'function') throw new Error('Installed Node lacks the synthetic zstd encoder')
            if (entry.zstdMemberSplitAt !== undefined) {
                const split = entry.zstdMemberSplitAt
                if (!Number.isInteger(split) || split <= 0 || split >= encoded.length) throw new Error('Invalid synthetic zstd member split')
                const first = zlib.zstdCompressSync(encoded.subarray(0, split))
                const second = entry.zstdSecondWindowLog === undefined ? zlib.zstdCompressSync(encoded.subarray(split))
                    : await streamedZstd(encoded.subarray(split), entry.zstdSecondWindowLog, signal)
                const skipped = entry.zstdSkippable ? Buffer.from('502a4d1800000000', 'hex') : Buffer.alloc(0)
                encoded = Buffer.concat([first, skipped, second])
            } else if (entry.zstdWindowLog !== undefined) encoded = await streamedZstd(encoded, entry.zstdWindowLog, signal)
            else encoded = zlib.zstdCompressSync(encoded)
        } else throw new Error('Unknown synthetic encoding')
    }
    if (entry.truncateBytes !== undefined) {
        if (!Number.isInteger(entry.truncateBytes) || entry.truncateBytes <= 0 || entry.truncateBytes >= encoded.length) throw new Error('Invalid synthetic truncation')
        encoded = encoded.subarray(0, encoded.length - entry.truncateBytes)
    }
    if (entry.corruptGzipCrc32) {
        if (codings.length !== 1 || codings[0] !== 'gzip' || encoded.length < 18 || encoded[0] !== 0x1f || encoded[1] !== 0x8b) throw new Error('CRC probe requires a generated gzip member')
        encoded = Buffer.from(encoded)
        encoded[encoded.length - 8] ^= 0xff
    }
    if (encoded.length > 256 * 1024) throw new Error('Synthetic encoded body bound exceeded')
    let windowEvidence
    const requestedWindow = entry.zstdWindowLog ?? entry.zstdSecondWindowLog
    if (requestedWindow !== undefined) {
        const controls = []
        for (const cap of [10, requestedWindow]) {
            signal.throwIfAborted()
            try {
                const decoded = zlib.zstdDecompressSync(encoded, { params: { [zlib.constants.ZSTD_d_windowLogMax]: cap } })
                controls.push({ cap, decodedMatchesInput: decoded.equals(raw) })
            } catch (error) {
                controls.push({ cap, error: { name: error?.name, code: error?.code, message: error?.message } })
            }
        }
        windowEvidence = { requestedWindow, controls, encodedHex: encoded.toString('hex'),
            proof: 'Requested encoder parameter is not an asserted header window; exact bytes and standard decoder controls require review' }
    }
    return { bytes: encoded, evidence: { encodedBytes: encoded.length,
        encodedSha256: createHash('sha256').update(encoded).digest('hex'), windowEvidence } }
}

function contentEncoding(entry) {
    if (Object.hasOwn(entry, 'contentEncoding')) return entry.contentEncoding
    return (entry.encodings ?? (entry.encoding ? [entry.encoding] : [])).map(coding => coding === 'deflate-raw' ? 'deflate' : coding).join(', ') || null
}

export async function createOllamaFixtureServer(entry, options = {}) {
    const preparation = new AbortController()
    const timer = setTimeout(() => preparation.abort(new Error('Synthetic fixture preparation deadline exceeded')), 15000)
    let preparedResponse
    try { preparedResponse = await responseBytes(entry, preparation.signal) }
    finally { clearTimeout(timer) }
    const requests = []
    const preflights = []
    const requestSeen = deferred()
    const release = deferred()
    const closed = deferred()
    let responseClosedWithoutEnd = false
    let origin = ''
    let serverFault
    let disposal
    const server = createServer((request, response) => {
        response.on('error', () => {})
        void handle(request, response).catch(error => {
            serverFault = error
            requestSeen.resolve()
            release.resolve()
            response.destroy()
        })
    })
    async function handle(request, response) {
        const path = request.url ?? '/'
        const asset = options.assets?.get(path)
        if (asset && request.method === 'GET') {
            response.writeHead(200, { 'Content-Type': asset.contentType })
            response.end(asset.body)
            return
        }
        const apiPath = entry.expectedPath ?? '/api/chat'
        const isRedirectTarget = Boolean(entry.redirect) && path === '/redirect-target/api/chat'
        const allowedMethods = isRedirectTarget ? ['POST', 'GET'] : ['POST']
        const allowedPath = path === apiPath || isRedirectTarget
        const requestOrigin = request.headers.origin
        const allowedOrigin = typeof requestOrigin === 'string' && [origin, options.corsOrigin].includes(requestOrigin)
        const reject = () => {
            serverFault = new Error('Request outside the synthetic fixture origin/path/method/header contract')
            response.writeHead(403)
            response.end()
        }
        if (request.method === 'OPTIONS') {
            const requestedMethod = request.headers['access-control-request-method']
            const requestedHeaders = String(request.headers['access-control-request-headers'] ?? '')
                .split(',').map(header => header.trim().toLowerCase()).filter(Boolean)
            const allowedHeaders = new Set(['accept', 'content-type', 'user-agent'])
            const accepted = allowedPath && allowedOrigin && allowedMethods.includes(requestedMethod)
                && requestedHeaders.every(header => allowedHeaders.has(header))
            preflights.push({ path, origin: requestOrigin, requestedMethod, requestedHeaders, accepted })
            if (!accepted) { reject(); return }
            response.writeHead(204, {
                'Access-Control-Allow-Origin': requestOrigin,
                'Access-Control-Allow-Methods': allowedMethods.join(', '),
                'Access-Control-Allow-Headers': requestedHeaders.join(', '),
                Vary: 'Origin',
            })
            response.end()
            return
        }
        if (!allowedPath || !allowedMethods.includes(request.method) || (requestOrigin !== undefined && !allowedOrigin)) { reject(); return }
        if (allowedOrigin) {
            response.setHeader('Access-Control-Allow-Origin', requestOrigin)
            response.setHeader('Vary', 'Origin')
        }
        const chunks = []
        let size = 0
        for await (const chunk of request) {
            const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)
            size += bytes.length
            if (size > 64 * 1024) throw new Error('Synthetic request exceeds its bound')
            chunks.push(bytes)
        }
        const text = Buffer.concat(chunks).toString('utf8')
        const headers = {}
        for (const key of ['content-type', 'accept', 'authorization', 'user-agent', 'accept-encoding']) {
            const value = request.headers[key]
            if (typeof value === 'string') headers[key] = value
        }
        requests.push({ method: request.method ?? '', path: request.url ?? '', headers, body: text ? JSON.parse(text) : null })
        requestSeen.resolve()
        if (entry.redirect && (requests.length === 1 || entry.redirectLoop)) {
            if (requests.length > 24) throw new Error('Synthetic redirect request bound exceeded')
            response.writeHead(entry.redirect, { Location: `${origin}/redirect-target/api/chat` })
            response.end()
            return
        }
        response.on('close', () => {
            responseClosedWithoutEnd = !response.writableFinished
            closed.resolve()
        })
        if (entry.lifecycle === 'before-headers') { await release.promise; return }
        response.statusCode = entry.status ?? 200
        if (entry.statusText !== undefined) response.statusMessage = entry.statusText
        response.setHeader('Content-Type', entry.contentType ?? 'application/json')
        const codingHeader = contentEncoding(entry)
        if (codingHeader !== null) response.setHeader('Content-Encoding', codingHeader)
        response.flushHeaders()
        const bytes = preparedResponse.bytes
        const step = entry.fragmentBytes ?? Math.max(bytes.length, 1)
        if (step <= 0) throw new Error('Expected a positive synthetic fragment size')
        for (let offset = 0; offset < bytes.length && !response.destroyed; offset += step) {
            response.write(bytes.subarray(offset, offset + step))
            if (entry.fragmentBytes) await immediate()
        }
        if (entry.lifecycle && entry.lifecycle !== 'pre-abort') {
            await release.promise
            if (entry.lifecycle === 'body-failure' && !response.destroyed) response.destroy(new Error('Synthetic connection failure'))
            return
        }
        response.end()
    }
    await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(0, '127.0.0.1', () => { server.removeListener('error', reject); resolve() })
    })
    const address = server.address()
    if (!address || typeof address === 'string') throw new Error('Expected ephemeral loopback address')
    origin = `http://127.0.0.1:${address.port}`
    const revoke = options.allowOrigin?.(origin) ?? (() => {})
    return {
        origin, port: address.port, requests, preflights, responseEvidence: preparedResponse.evidence, requestSeen, release, closed,
        get responseClosedWithoutEnd() { return responseClosedWithoutEnd },
        assertHealthy() { if (serverFault) throw serverFault },
        dispose() {
            if (!disposal) disposal = (async () => {
                release.resolve()
                const stopped = once(server, 'close')
                server.close()
                server.closeAllConnections()
                try { await stopped } finally { revoke() }
            })()
            return disposal
        },
    }
}


function boundedOutput(root, input) {
    if (!input) throw new Error('Pass --output with a file under .tmp/native-ollama-transport')
    const output = resolve(input)
    const local = relative(resolve(root, '.tmp/native-ollama-transport'), output)
    if (!local || isAbsolute(local) || local === '..' || local.startsWith(`..${sep}`)) throw new Error('Browser oracle output must stay under .tmp/native-ollama-transport')
    return output
}

function identity(path) {
    return { file: basename(path), sha256: createHash('sha256').update(readFileSync(path)).digest('hex') }
}

async function deadline(task, milliseconds) {
    let timer
    try {
        return await Promise.race([task, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Browser diagnostic case deadline exceeded')), milliseconds) })])
    } finally { clearTimeout(timer) }
}

export async function runBrowserOracle({ output: suppliedOutput, caseIds = [] } = {}) {
    const root = resolve(fileURLToPath(new URL('../..', import.meta.url)))
    const output = boundedOutput(root, suppliedOutput)
    const fixturePath = resolve(root, 'tests/fixtures/native-generation/ollama-transport.json')
    const fixture = JSON.parse(readFileSync(fixturePath, 'utf8'))
    const sdkPath = resolve(root, 'node_modules/ollama/dist/browser.mjs')
    const sdkPackage = JSON.parse(readFileSync(resolve(root, 'node_modules/ollama/package.json'), 'utf8'))
    const sdkRequire = createRequire(realpathSync(sdkPath))
    const polyfillPackagePath = sdkRequire.resolve('whatwg-fetch/package.json')
    const polyfillPackage = JSON.parse(readFileSync(polyfillPackagePath, 'utf8'))
    if (polyfillPackage.module !== './fetch.js') throw new Error('Expected installed whatwg-fetch ESM entry ./fetch.js')
    const polyfillPath = resolve(dirname(polyfillPackagePath), polyfillPackage.module)
    const sdk = readFileSync(sdkPath)
    if (sdkPackage.version !== '0.6.4' || fixture.sdkVersion !== sdkPackage.version || fixture.schema !== 1) throw new Error('Browser oracle requires fixture schema 1 and installed SDK 0.6.4')
    if (new Set(fixture.cases.map(entry => entry.id)).size !== fixture.cases.length) throw new Error('Duplicate fixture IDs')
    const selected = caseIds.length ? fixture.cases.filter(entry => caseIds.includes(entry.id)) : fixture.cases
    if (!selected.length || caseIds.some(id => !selected.some(entry => entry.id === id))) throw new Error('Unknown or empty browser case selection')
    const sources = { fixture: identity(fixturePath), driver: identity(fileURLToPath(import.meta.url)),
        productTransformOracle: identity(resolve(root, 'benchmarks/generation-send/ollama-transport-oracle.test.ts')),
        sdk: { ...identity(sdkPath), version: sdkPackage.version },
        fetchPolyfill: { ...identity(polyfillPath), version: polyfillPackage.version, entry: polyfillPackage.module } }
    const { chromium } = await import('@playwright/test')
    const browser = await chromium.launch({ headless: true, args: ['--disable-background-networking', '--disable-component-update', '--no-first-run'] })
    let browserIdentity
    try {
        const browserSession = await browser.newBrowserCDPSession()
        browserIdentity = await browserSession.send('Browser.getVersion')
    } catch (error) { await browser.close(); throw error }
    const result = {
        schema: 1, phase: 'unfrozen-real-browser-sdk-diagnostic', complete: false,
        fixtureEncodingRuntime: { node: process.versions.node, zlib: process.versions.zlib, brotli: process.versions.brotli, zstd: process.versions.zstd ?? null },
        selectedCases: selected.map(entry => entry.id), browser: browserIdentity,
        sources,

        proofBoundary: { rawSdkOnly: true, productAdapterExecuted: false, nativeAcceptance: false,
            cleanup: 'Driver aborts iterator after raw observation; this does not establish SDK or product disposal behavior',
            cancellation: 'Caller and SDK signals joined by driver Fetch wrapper; no product abort-scope claim' },
        cases: [],
    }
    try {
        for (const entry of selected) {
            const assets = new Map([
                ['/oracle.html', { contentType: 'text/html; charset=utf-8', body: Buffer.from('<!doctype html><meta charset="utf-8"><link rel="icon" href="data:,"><script type="importmap">{"imports":{"whatwg-fetch":"/sdk/whatwg-fetch.js"}}</script>') }],
                ['/sdk/browser.mjs', { contentType: 'application/javascript', body: sdk }],
                ['/sdk/whatwg-fetch.js', { contentType: 'application/javascript', body: readFileSync(polyfillPath) }],
            ])
            let controls
            let context
            const blocked = []
            try {
                controls = await createOllamaFixtureServer(entry, { assets })
                context = await browser.newContext({ serviceWorkers: 'block' })
                const allowedPaths = new Set([...assets.keys(), entry.expectedPath ?? '/api/chat', '/redirect-target/api/chat'])
                await context.route('**/*', async route => {
                    const request = route.request()
                    const url = new URL(request.url())
                    if (isRealmUrl(url.href) || isRealmPath(url.pathname) || url.origin !== controls.origin || !allowedPaths.has(url.pathname)) {
                        blocked.push({ origin: url.origin, path: url.pathname, reason: 'outside-owned-allowlist-or-Realm' })
                        await route.abort('blockedbyclient')
                        return
                    }
                    await route.continue()
                })
                const page = await context.newPage()
                await page.exposeFunction('__ollamaFirstFrame', () => { if (entry.lifecycle === 'body-failure') controls.release.resolve() })
                await page.goto(`${controls.origin}/oracle.html`, { waitUntil: 'load', timeout: 15000 })
                const host = (entry.hostTemplate ?? '{origin}').replaceAll('{origin}', controls.origin).replaceAll('{port}', String(controls.port))
                const body = { model: fixture.request.model, messages: fixture.request.wireMessages, stream: entry.stream ?? false,
                    ...(Object.hasOwn(entry, 'expectedThink') ? { think: entry.expectedThink } : {}) }
                const pending = page.evaluate(async ({ sdkURL, polyfillURL, host, body, lifecycle }) => {
                    const nativeFetch = globalThis.fetch
                    const { Ollama } = await import(sdkURL)
                    if (globalThis.fetch !== nativeFetch) throw new Error('SDK import replaced native Chromium Fetch')
                    const caller = globalThis.__ollamaCaller = new AbortController()
                    const frames = []
                    let iterator
                    if (lifecycle === 'pre-abort') caller.abort()
                    const fetchObservations = []
                    const observerFaults = []
                    const joinedSignalFetch = (input, init) => nativeFetch(input, { ...init,
                        signal: init?.signal ? AbortSignal.any([caller.signal, init.signal]) : caller.signal })
                    function observingFetch(...args) {
                        let promise
                        try { promise = Reflect.apply(joinedSignalFetch, this, args) }
                        catch (error) {
                            try { fetchObservations.push({ outcome: 'synchronous-throw', error: { name: error?.name ?? typeof error, message: error?.message ?? String(error) } }) }
                            catch { observerFaults.push('synchronous-error-observation-failed') }
                            throw error
                        }
                        try {
                            void promise.then(response => {
                                try {
                                    const responseBody = response.body
                                    fetchObservations.push({ outcome: 'response', status: response.status, statusText: response.statusText,
                                        url: response.url, redirected: response.redirected, type: response.type,
                                        bodyIsNull: responseBody === null, bodyUsed: response.bodyUsed, bodyLocked: responseBody?.locked ?? null,
                                        headers: Object.fromEntries(['content-type', 'content-encoding', 'content-length']
                                            .map(name => [name, response.headers.get(name)])) })
                                } catch { observerFaults.push('response-metadata-observation-failed') }
                            }, error => {
                                try { fetchObservations.push({ outcome: 'rejection', error: { name: error?.name ?? typeof error, message: error?.message ?? String(error) } }) }
                                catch { observerFaults.push('rejection-observation-failed') }
                            }).catch(() => { observerFaults.push('observer-branch-failed') })
                        } catch { observerFaults.push('observer-attachment-failed') }
                        return promise
                    }
                    const ponyfill = await import(polyfillURL)
                    const defaultClient = new Ollama({ host })
                    const client = new Ollama({ host, fetch: observingFetch })
                    const fetchSelection = { providedClientUsesObserver: client.fetch === observingFetch,
                        defaultClientUsesCapturedNative: defaultClient.fetch === nativeFetch,
                        defaultClientUsesPonyfill: defaultClient.fetch === ponyfill.fetch,
                        globalStillCapturedNative: globalThis.fetch === nativeFetch,
                        capturedNativeIsPonyfill: nativeFetch === ponyfill.fetch }
                    const diagnostics = { fetchSelection, fetchObservations, observerFaults }
                    try {
                        const response = await client.chat(body)
                        if (!body.stream) return { outcome: 'buffered-json', value: response, frames, diagnostics }
                        iterator = response
                        for await (const frame of response) {
                            frames.push(frame)
                            if (frames.length === 1) {
                                await globalThis.__ollamaFirstFrame()
                                if (lifecycle === 'caller-cancel') caller.abort()
                                if (lifecycle === 'consumer-cancel') iterator.abort()
                            }
                        }
                        return { outcome: 'stream-complete', frames, diagnostics }
                    } catch (error) {
                        return { outcome: 'error', frames, diagnostics, error: { name: error?.name ?? typeof error,
                            message: error?.message ?? String(error),
                            ...(typeof error?.status_code === 'number' ? { statusCode: error.status_code } : {}),
                            ...(error && Object.hasOwn(error, 'error') ? { sdkError: error.error } : {}) } }
                    } finally { iterator?.abort(); caller.abort() }
                }, { sdkURL: `${controls.origin}/sdk/browser.mjs`, polyfillURL: `${controls.origin}/sdk/whatwg-fetch.js`, host, body, lifecycle: entry.lifecycle })
                void pending.catch(() => {})
                const observation = await deadline((async () => {
                    if (entry.lifecycle === 'before-headers') {
                        await Promise.race([controls.requestSeen.promise, pending.then(() => { throw new Error('Raw SDK settled before held-header condition') })])
                        await page.evaluate(() => globalThis.__ollamaCaller.abort())
                    }
                    return await pending
                })(), 15000)
                controls.assertHealthy()
                result.cases.push({ id: entry.id, gate: entry.gate, input: encodeDiagnosticValue(body),
                    responseEvidence: controls.responseEvidence, wire: encodeDiagnosticValue(controls.requests), preflights: encodeDiagnosticValue(controls.preflights), rawSdk: encodeDiagnosticValue(observation), blockedRequests: blocked,
                    responseClosedWithoutEnd: controls.responseClosedWithoutEnd })
                if (blocked.length) throw new Error(`Unexpected browser requests in synthetic case ${entry.id}`)
            } catch (error) {
                result.failure = { case: entry.id, name: error?.name ?? typeof error, message: String(error?.message ?? error) }
                throw error
            } finally {
                try { await context?.close() } finally { await controls?.dispose() }
            }
        }
        result.complete = true
        return result
    } finally {
        try { await browser.close() } finally {
            mkdirSync(dirname(output), { recursive: true })
            writeFileSync(output, JSON.stringify(result, null, 2) + '\n')
        }
    }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    const args = process.argv.slice(2)
    let output
    const caseIds = []
    for (let index = 0; index < args.length; index++) {
        if (args[index] === '--output') output = args[++index]
        else if (args[index] === '--case') caseIds.push(args[++index])
        else throw new Error('Usage: node benchmarks/generation-send/ollama-browser-oracle.mjs --output <private-json> [--case <id>]')
    }
    runBrowserOracle({ output, caseIds }).then(result => {
        process.stdout.write(JSON.stringify({ complete: result.complete, cases: result.cases.length, phase: result.phase }) + '\n')
    }).catch(error => { process.stderr.write(String(error?.message ?? error) + '\n'); process.exitCode = 1 })
}
