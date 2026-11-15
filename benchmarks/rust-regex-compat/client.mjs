import { spawn } from 'node:child_process'
import { Worker } from 'node:worker_threads'

export function nativeClient(binary) {
    const child = spawn(binary, [], { stdio: ['pipe', 'pipe', 'ignore'], windowsHide: true })
    let buffer = '', pending, stopped = false, timeoutRequested = false, requestId = 0
    const exited = new Promise(resolve => child.once('close', (code, signal) => {
        stopped = true
        if (pending) { clearTimeout(pending.timer); pending.resolve({ status: timeoutRequested ? 'externalTimeout' : 'processFailure', code, signal }); pending = undefined }
        resolve()
    }))
    child.on('error', () => { stopped = true })
    child.stdin.on('error', () => {})
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => {
        buffer += chunk
        if (buffer.length > 32 * 1024 * 1024) { child.kill(); return }
        const end = buffer.indexOf('\n')
        if (end < 0) return
        const line = buffer.slice(0, end); buffer = buffer.slice(end + 1)
        if (!pending) { child.kill(); return }
        let result
        try { result = JSON.parse(line) } catch { child.kill(); return }
        if (result.requestId !== pending.requestId) { child.kill(); return }
        delete result.requestId
        const active = pending; pending = undefined; clearTimeout(active.timer); active.resolve(result)
    })
    return {
        async send(request, timeoutMs = 3000) {
            if (pending) throw Error('Concurrent native request')
            if (stopped) throw Error('Native process is closed')
            return new Promise(resolve => {
                const timer = setTimeout(() => { timeoutRequested = true; child.kill() }, timeoutMs)
                pending = { timer, resolve, requestId: ++requestId }
                child.stdin.write(JSON.stringify({ request_id: requestId, request }) + '\n')
            })
        },
        async close() { if (!stopped) { child.stdin.end(); child.kill() }; await exited },
    }
}

export async function oracleClient() {
    const worker = new Worker(new URL('./oracle.worker.mjs', import.meta.url))
    await new Promise((resolve, reject) => { worker.once('message', resolve); worker.once('error', reject) })
    let stopped = false
    return {
        async send(request, timeoutMs = 3000) {
            if (stopped) throw Error('Oracle worker is closed')
            return new Promise((resolve, reject) => {
                const cleanup = () => { clearTimeout(timer); worker.off('message', done); worker.off('error', fail) }
                const done = value => { cleanup(); resolve(value) }
                const fail = error => { cleanup(); reject(error) }
                const timer = setTimeout(async () => { cleanup(); stopped = true; await worker.terminate(); resolve({ status: 'externalTimeout' }) }, timeoutMs)
                worker.once('message', done); worker.once('error', fail); worker.postMessage(request)
            })
        },
        async close() { stopped = true; await worker.terminate() },
    }
}
