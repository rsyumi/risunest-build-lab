import { test } from 'node:test'
import assert from 'node:assert/strict'
import { startOllamaFixture } from './ollamaFixture.mjs'

const post = (fixture, body, init = {}) => fetch(`${fixture.origin}/api/chat`, {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body), ...init,
})

test('the Ollama fixture answers a buffered chat on loopback and counts each request', async () => {
    const fixture = await startOllamaFixture()
    try {
        assert.match(fixture.origin, /^http:\/\/127\.0\.0\.1:\d+$/)
        fixture.setReply({ content: 'Hi there' })
        const response = await post(fixture, { model: 'm', stream: false })
        assert.equal(response.status, 200)
        assert.equal((await response.json()).message.content, 'Hi there')
        assert.equal(fixture.requestCount, 1)
        assert.deepEqual(fixture.requests[0].json, { model: 'm', stream: false })

        const other = await fetch(`${fixture.origin}/api/tags`)
        assert.equal(other.status, 404)
        assert.equal(fixture.requestCount, 2)
        assert.equal(fixture.abortedCount, 0)
    } finally { await fixture.close() }
})

test('the Ollama fixture returns the configured status, headers and body', async () => {
    const fixture = await startOllamaFixture()
    try {
        fixture.setReply({ status: 500, headers: { 'x-fixture': 'one' }, body: { error: 'boom' } })
        const response = await post(fixture, {})
        assert.equal(response.status, 500)
        assert.equal(response.headers.get('x-fixture'), 'one')
        assert.deepEqual(await response.json(), { error: 'boom' })

        fixture.setReply({ status: 200, body: 'not json' })
        assert.equal(await (await post(fixture, {})).text(), 'not json')
    } finally { await fixture.close() }
})

test('the Ollama fixture keeps a reply back until it is released', async () => {
    const fixture = await startOllamaFixture()
    try {
        fixture.hold()
        const pending = post(fixture, {})
        await fixture.waitForRequests(1)
        assert.equal(fixture.heldCount, 1)
        const winner = await Promise.race([pending.then(() => 'replied'), new Promise((resolve) => setTimeout(resolve, 50, 'waiting'))])
        assert.equal(winner, 'waiting')

        fixture.release()
        assert.equal((await pending).status, 200)
        assert.equal(fixture.heldCount, 0)
        assert.equal(fixture.requestCount, 1)
    } finally { await fixture.close() }
})

test('the Ollama fixture records a client that drops a held request', async () => {
    const fixture = await startOllamaFixture()
    try {
        fixture.hold()
        const controller = new AbortController()
        const pending = post(fixture, {}, { signal: controller.signal })
        await fixture.waitForRequests(1)
        controller.abort()
        await assert.rejects(pending)
        for (let wait = 0; wait < 100 && fixture.abortedCount === 0; wait++) await new Promise((resolve) => setTimeout(resolve, 10))
        assert.equal(fixture.abortedCount, 1)
        assert.equal(fixture.heldCount, 0)
    } finally { await fixture.close() }
})

test('a page drives the Ollama fixture over uncounted control requests', async () => {
    const fixture = await startOllamaFixture({ control: true })
    const control = async (action, body) => {
        const response = await fetch(`${fixture.origin}/__fixture/${action}`, body === undefined && action === 'state' ? {}
            : { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body ?? {}) })
        assert.equal(response.headers.get('access-control-allow-origin'), '*')
        return { status: response.status, body: await response.json() }
    }
    try {
        const preflight = await fetch(`${fixture.origin}/__fixture/reply`, { method: 'OPTIONS' })
        assert.equal(preflight.status, 204)
        assert.equal(preflight.headers.get('access-control-allow-headers'), 'content-type')
        await control('reply', { status: 500, body: { error: 'Synthetic failure' } })
        assert.equal((await post(fixture, {})).status, 500)

        await control('hold')
        const pending = post(fixture, {})
        await fixture.waitForRequests(2)
        assert.deepEqual((await control('state')).body, { requestCount: 2, heldCount: 1, abortedCount: 0, bodyStartedCount: 0 })
        assert.equal((await control('reset')).status, 409)
        await control('release')
        assert.equal((await pending).status, 500)

        assert.deepEqual((await control('reset')).body, { requestCount: 0, heldCount: 0, abortedCount: 0, bodyStartedCount: 0 })
        assert.equal((await (await post(fixture, {})).json()).message.content, 'Hello.')
        assert.equal(fixture.requestCount, 1)
    } finally { await fixture.close() }

    const plain = await startOllamaFixture()
    try {
        assert.equal((await fetch(`${plain.origin}/__fixture/state`)).status, 404)
        assert.equal(plain.requestCount, 1)
    } finally { await plain.close() }
})

test('optional chat CORS accepts exactly the supplied origin and counts only the provider POST', async () => {
    const origin = 'http://tauri.localhost'
    const fixture = await startOllamaFixture({ corsOrigin: origin })
    const preflight = (headers = {}) => fetch(`${fixture.origin}/api/chat`, { method: 'OPTIONS', headers: {
        origin, 'access-control-request-method': 'POST', 'access-control-request-headers': 'content-type', ...headers,
    } })
    try {
        const response = await preflight()
        assert.equal(response.status, 204)
        assert.equal(response.headers.get('access-control-allow-origin'), origin)
        assert.equal(response.headers.get('access-control-allow-methods'), 'POST')
        assert.equal(response.headers.get('access-control-allow-headers'), 'content-type')
        assert.equal(fixture.requestCount, 0)
        for (const headers of [
            { origin: 'http://other.invalid' }, { 'access-control-request-method': 'DELETE' },
            { 'access-control-request-headers': 'authorization' },
        ]) {
            const denied = await preflight(headers)
            assert.equal(denied.status, 403)
            assert.equal(denied.headers.get('access-control-allow-origin'), null)
        }
        const deniedPost = await post(fixture, {}, { headers: { origin: 'http://other.invalid', 'content-type': 'application/json' } })
        assert.equal(deniedPost.status, 403)
        assert.equal(deniedPost.headers.get('access-control-allow-origin'), null)
        assert.equal(fixture.requestCount, 0)
        const accepted = await post(fixture, { stream: false }, { headers: { origin, 'content-type': 'application/json' } })
        assert.equal(accepted.status, 200)
        assert.equal(accepted.headers.get('access-control-allow-origin'), origin)
        assert.equal((await accepted.json()).message.content, 'Hello.')
        assert.equal(fixture.requestCount, 1)
        assert.equal(fixture.requests[0].method, 'POST')
    } finally { await fixture.close() }
    await assert.rejects(() => startOllamaFixture({ corsOrigin: '*' }))
    await assert.rejects(() => startOllamaFixture({ corsOrigin: 'https://tauri.localhost/path' }))
})


test('a body hold delivers real headers and a nonempty prefix, then releases only the original remainder', { timeout: 5_000 }, async () => {
    const fixture = await startOllamaFixture({ control: true, corsOrigin: 'http://tauri.localhost' })
    const payload = JSON.stringify({ message: { content: 'Synthetic 한글 reply.' }, done: true })
    try {
        fixture.setReply({ headers: { 'x-fixture': 'partial' }, body: payload })
        for (const prefix of [0, -1, 1.5, Buffer.byteLength(payload), Number.NaN]) {
            assert.throws(() => fixture.holdBody(prefix), /nonempty prefix and remainder/)
        }
        fixture.holdBody(21)
        const response = await post(fixture, {}, { headers: { origin: 'http://tauri.localhost' } })
        assert.equal(response.status, 200)
        assert.equal(response.headers.get('x-fixture'), 'partial')
        assert.equal(response.headers.get('access-control-allow-origin'), 'http://tauri.localhost')
        const reader = response.body.getReader()
        const prefix = await reader.read()
        assert.equal(prefix.done, false)
        assert.deepEqual(Buffer.from(prefix.value), Buffer.from(payload).subarray(0, 21))
        await fixture.waitForBodyStart()
        const remainder = reader.read()
        let finished = false
        void remainder.then(() => { finished = true })
        const state = await (await fetch(`${fixture.origin}/__fixture/state`)).json()
        assert.deepEqual(state, { requestCount: 1, heldCount: 1, abortedCount: 0, bodyStartedCount: 1 })
        assert.equal(finished, false, 'The body cannot complete while its remainder is held')
        fixture.setReply({ status: 500, body: 'Changed later reply' })
        fixture.release()
        fixture.release()
        const chunks = [Buffer.from(prefix.value)]
        let next = await remainder
        while (!next.done) { chunks.push(Buffer.from(next.value)); next = await reader.read() }
        assert.deepEqual(Buffer.concat(chunks), Buffer.from(payload), 'Release neither repeats the prefix nor changes the reply')
        assert.equal(fixture.heldCount, 0)
        assert.equal(fixture.requestCount, 1)
        assert.equal(fixture.abortedCount, 0)
    } finally { await fixture.close() }
})

test('aborting a partial body removes its held remainder before release', { timeout: 5_000 }, async () => {
    const fixture = await startOllamaFixture()
    try {
        fixture.holdBody(8)
        const controller = new AbortController()
        const response = await post(fixture, {}, { signal: controller.signal })
        const reader = response.body.getReader()
        assert.ok((await reader.read()).value.length > 0)
        await fixture.waitForBodyStart()
        controller.abort()
        await assert.rejects(reader.read())
        const deadline = Date.now() + 2_000
        while (fixture.abortedCount === 0 && Date.now() < deadline) await new Promise(resolve => setImmediate(resolve))
        assert.equal(fixture.abortedCount, 1)
        assert.equal(fixture.heldCount, 0)
        fixture.release()
        assert.equal(fixture.requestCount, 1)
    } finally { await fixture.close() }
})

test('body controls reject invalid holds, fence reset until release, and clean pending work on reset and close', { timeout: 5_000 }, async () => {
    const fixture = await startOllamaFixture({ control: true })
    const control = async (action, body = {}) => fetch(`${fixture.origin}/__fixture/${action}`, {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
    })
    let closed = false
    try {
        assert.equal((await control('hold-body', { prefixBytes: 0 })).status, 400)
        assert.equal((await control('hold-body', { prefixBytes: 8 })).status, 200)
        const response = await post(fixture, {})
        const reader = response.body.getReader()
        assert.ok((await reader.read()).value.length > 0)
        await fixture.waitForBodyStart()
        assert.equal((await control('reset')).status, 409)
        await control('release')
        while (!(await reader.read()).done) { /* consume the remaining body */ }
        const resetWait = assert.rejects(fixture.waitForBodyStart(2), /Fixture reset/)
        const reset = await control('reset')
        assert.equal(reset.status, 200)
        assert.deepEqual(await reset.json(), { requestCount: 0, heldCount: 0, abortedCount: 0, bodyStartedCount: 0 })
        await resetWait
        fixture.holdBody(8)
        const later = await post(fixture, {})
        const laterReader = later.body.getReader()
        assert.ok((await laterReader.read()).value.length > 0)
        await fixture.waitForBodyStart()
        const bodyClosed = assert.rejects(laterReader.read())
        const waitClosed = assert.rejects(fixture.waitForBodyStart(2), /Fixture closed/)
        await fixture.close()
        closed = true
        await Promise.all([bodyClosed, waitClosed])
        assert.equal(fixture.heldCount, 0)
    } finally { if (!closed) await fixture.close() }
})
