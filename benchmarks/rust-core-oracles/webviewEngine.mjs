// Evaluates an expression in a WebView's JavaScript engine for the `*-webview.capture.mjs` scripts, which record what
// that engine's own Unicode tables decide.
//
//   --browser <executable>   starts a Chromium-family executable headless on about:blank with a temporary profile: on
//                            Windows, the Edge build whose version matches the WebView2 runtime ships the same engine
//                            files, while `msedgewebview2.exe` does not start on its own.
//   --cdp <http endpoint>    evaluates in the first page of an engine already serving the DevTools protocol, such as the
//                            agent app's WebView2.
import { spawn } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

export function option(name) {
    const index = process.argv.indexOf(name)
    return index < 0 ? undefined : process.argv[index + 1]
}

async function until(condition, what, timeout = 30_000) {
    const deadline = Date.now() + timeout
    for (;;) {
        const value = condition()
        if (value) return value
        if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
        await new Promise(resolve => setTimeout(resolve, 100))
    }
}

// Sends one DevTools command over a fresh socket; the reply is undefined when the socket closes first, as it does
// when the command closes the browser.
async function call(url, method, params = {}) {
    const socket = new WebSocket(url)
    await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject })
    try {
        const reply = await new Promise((resolve, reject) => {
            socket.onmessage = event => { const message = JSON.parse(event.data); if (message.id === 1) resolve(message) }
            socket.onerror = reject
            socket.onclose = () => resolve(undefined)
            socket.send(JSON.stringify({ id: 1, method, params }))
        })
        if (reply?.error) throw new Error(`${method}: ${JSON.stringify(reply.error)}`)
        return reply?.result
    } finally {
        if (socket.readyState === WebSocket.OPEN) socket.close()
    }
}

async function evaluate(endpoint, expression) {
    const targets = await (await fetch(`${endpoint}/json/list`)).json()
    const page = targets.find(target => target.type === 'page')
    if (!page) throw new Error(`no page target at ${endpoint}`)
    const result = await call(page.webSocketDebuggerUrl, 'Runtime.evaluate', { expression, returnByValue: true })
    if (!result || result.exceptionDetails) throw new Error(`Runtime.evaluate: ${JSON.stringify(result?.exceptionDetails)}`)
    return result.result.value
}

/** The value of `expression` in the engine the command line names with `--browser` or `--cdp`. */
export async function evaluateInEngine(expression, profilePrefix) {
    const browser = option('--browser')
    let endpoint = option('--cdp')
    if (!browser === !endpoint) throw new Error('pass exactly one of --browser <executable> or --cdp <http endpoint>')
    let child
    let profile
    // The browser target of a started executable, which can outlive the process `spawn` returns (Edge relaunches itself).
    let browserTarget
    try {
        if (browser) {
            profile = mkdtempSync(join(tmpdir(), profilePrefix))
            child = spawn(browser, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`, '--no-first-run',
                '--no-default-browser-check', '--disable-extensions', '--disable-background-networking', '--disable-component-update',
                '--disable-sync', 'about:blank'], { stdio: 'ignore' })
            const portFile = join(profile, 'DevToolsActivePort')
            // The port, then the browser target's path.
            const [port, path] = await until(() => {
                const lines = existsSync(portFile) ? readFileSync(portFile, 'utf8').split('\n').map(line => line.trim()).filter(Boolean) : []
                return lines.length >= 2 && lines
            }, 'DevToolsActivePort')
            endpoint = `http://127.0.0.1:${port}`
            browserTarget = `ws://127.0.0.1:${port}${path}`
        }
        return await evaluate(endpoint, expression)
    } finally {
        if (browserTarget) await call(browserTarget, 'Browser.close').catch(error => console.error(`Browser.close: ${error}`))
        if (child && child.exitCode === null && child.signalCode === null) {
            const exited = new Promise(resolve => child.once('exit', resolve))
            child.kill()
            await exited
        }
        // Helper processes hold files in the profile for a while after the browser closes.
        if (profile) {
            const deadline = Date.now() + 60_000
            for (;;) {
                try {
                    rmSync(profile, { recursive: true, force: true })
                    break
                } catch (error) {
                    if (Date.now() > deadline) {
                        console.error(`left the temporary profile ${profile}: ${error.code}`)
                        break
                    }
                    await new Promise(resolve => setTimeout(resolve, 500))
                }
            }
        }
    }
}
