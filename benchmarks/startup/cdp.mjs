import { REALM_BLOCKED_URL_PATTERNS, isRealmPath } from '../../scripts/realmBlocklist.mjs'
import { execFileSync } from 'node:child_process'
import { assertAgentDataRoot } from './runtime.mjs'

export function isAllowedStartupUrl(input) {
    try {
        const url = new URL(input)
        if (isRealmPath(url)) return false
        if (['data:', 'blob:', 'about:'].includes(url.protocol)) return true
        if (['tauri:', 'ipc:', 'asset:'].includes(url.protocol)) return true
        return ['http:', 'https:'].includes(url.protocol)
            && ['tauri.localhost', 'ipc.localhost', 'asset.localhost'].includes(url.hostname)
    } catch { return false }
}

export const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

// Never retain console events, exception details, request bodies, or page text.
export async function connect(port, identifier, timeoutMs = 120_000, profile) {
    if (!identifier.startsWith('RisuNest.phase3benchmark.')) throw new Error('Unsafe identifier')
    return connectVerifiedIdentity(port, identifier, timeoutMs, profile)
}

// Check the selected disposable AVD again before opening its WebView.
export async function connectSyntheticAndroid(port, android) {
    const name = execFileSync(android.adb, android.args(['emu', 'avd', 'name']), {
        env: android.env,
        encoding: 'utf8',
        timeout: 10_000,
        windowsHide: true,
    })
    android.assertAvd(name)
    return connectVerifiedIdentity(port, 'io.github.rsyumi.risunest', 120_000)
}

export async function connectVerifiedIdentity(port, identifier, timeoutMs = 120_000, profile) {
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
        let targets
        try {
            targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()
        } catch {
            await delay(100)
            continue
        }
        for (const target of targets.filter((item) => item.type === 'page')) {
            const ws = new WebSocket(target.webSocketDebuggerUrl)
            try {
                await new Promise((resolve, reject) => {
                    ws.onopen = resolve
                    ws.onerror = () => reject(new Error('CDP connection failed'))
                })
            } catch {
                ws.close()
                continue
            }
            let sequence = 0
            const pending = new Map()
            ws.onmessage = ({ data }) => {
                const message = JSON.parse(data)
                if (message.method === 'Fetch.requestPaused') {
                    const permitted = isAllowedStartupUrl(message.params.request.url)
                    void call(permitted ? 'Fetch.continueRequest' : 'Fetch.failRequest',
                        { requestId: message.params.requestId, ...(permitted ? {} : { errorReason: 'BlockedByClient' }) }).catch(() => {})
                    return
                }
                const entry = pending.get(message.id)
                if (!entry) return
                pending.delete(message.id)
                clearTimeout(entry.timer)
                if (message.error) entry.reject(new Error('CDP command failed'))
                else entry.resolve(message.result)
            }
            const call = (method, params = {}) =>
                new Promise((resolve, reject) => {
                    const id = ++sequence
                    const timer = setTimeout(() => {
                        pending.delete(id)
                        reject(new Error('CDP command timed out'))
                    }, timeoutMs)
                    pending.set(id, { resolve, reject, timer })
                    ws.send(JSON.stringify({ id, method, params }))
                })
            const evaluate = async (expression) => {
                const result = await call('Runtime.evaluate', {
                    expression,
                    awaitPromise: true,
                    returnByValue: true,
                })
                if (result.exceptionDetails) throw new Error('Synthetic page evaluation failed')
                return result.result.value
            }
            const close = () => {
                for (const entry of pending.values()) {
                    clearTimeout(entry.timer)
                    entry.reject(new Error('CDP closed'))
                }
                pending.clear()
                ws.close()
            }
            await call('Network.enable')
            await call('Network.setBlockedURLs', {
                urls: [
                    ...REALM_BLOCKED_URL_PATTERNS,
                    '*update.rsyumi.workers.dev/translator/prompt-presets.json*',
                ],
            })
            if (profile) await call('Fetch.enable', { patterns: [{ urlPattern: '*', requestStage: 'Request' }] })
            await call('Runtime.enable')
            await call('Page.enable')
            try {
                const matches = await evaluate(`(async () => {
                    const invoke = window.__TAURI_INTERNALS__?.invoke;
                    return !!invoke && await invoke('plugin:app|identifier') === ${JSON.stringify(identifier)};
                })()`)
                if (matches) {
                    if (profile) {
                        try {
                            const roots = await evaluate("window.__TAURI_INTERNALS__.invoke('app_paths_roots')")
                            assertAgentDataRoot(roots?.data, profile)
                        } catch {
                            close()
                            throw new Error('Native synthetic data root mismatch')
                        }
                    }
                    return { call, evaluate, close }
                }
            } catch (error) {
                if (error.message === 'Native synthetic data root mismatch') throw error
                /* No page data is exposed when identity cannot be established. */
            }
            close()
        }
        await delay(100)
    }
    throw new Error('Verified isolated CDP target unavailable')
}

export async function waitForInteractive(client, timeoutMs = 120_000) {
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
        if (await client.evaluate(`performance.getEntriesByName('boot:interactive').length > 0`))
            return
        await delay(100)
    }
    throw new Error('Synthetic startup timed out')
}

export const instrumentation = `(() => {
    performance.mark('startup:observer');
    const state = window.__startupMetrics = { calls: [], longTasks: [], operations: [], phases: [], active: {}, elapsedSeen: false, firstRevision: null, interaction: null };
    state.peakHeapBytes = 0; state.mediaInFlight = 0; state.maxMediaInFlight = 0; state.cleanChunksReason = 0;
    const heap = () => state.peakHeapBytes = Math.max(state.peakHeapBytes, performance.memory?.usedJSHeapSize ?? 0);
    window.__startupBegin = stage => { state.active[stage] = (state.active[stage] ?? 0) + 1; heap(); };
    window.__startupRecord = (stage, start, success) => {
        state.active[stage] = Math.max(0, (state.active[stage] ?? 1) - 1);
        heap();
        if (stage === 'store-commit' && state.operations.length >= 10000) {
            state.observerFailure = 'recording-limit';
            return;
        }
        if (performance.now() < 120000 || stage === 'store-commit')
            state.operations.push({stage, start, ms: performance.now() - start, success});
    };
    window.__startupTrackPromise = (stage, start, promise) => {
        window.__startupBegin(stage);
        promise.then(() => window.__startupRecord(stage, start, true),
            () => window.__startupRecord(stage, start, false));
    };
    if (localStorage.getItem('startupInteract') !== 'false') {
        const frame = () => new Promise(resolve => requestAnimationFrame(() => setTimeout(resolve, 0)));
        new PerformanceObserver(list => {
            if (!list.getEntries().some(e => e.name === 'boot:interactive') || state.interaction) return;
            state.interaction = {selectionMs: null, inputMs: null, scrollMs: null, success: false};
            void (async () => {
                const interactive = performance.getEntriesByName('boot:interactive')[0].startTime;
                await frame();
                let avatar, sidebarOpened = false;
                while (performance.now() - interactive < 5000) {
                    avatar = document.querySelector('[data-char-id="synthetic-0"]');
                    if (avatar && avatar.getBoundingClientRect().width > 0) break;
                    const sidebarButton = document.querySelector('button.absolute.top-3.left-0.border-borderc');
                    if (sidebarButton && !sidebarOpened) { sidebarButton.click(); sidebarOpened = true; }
                    await frame();
                }
                if (!avatar || avatar.getBoundingClientRect().width === 0) { state.interaction.failure = 1; return; }
                state.interaction.firstUiReadyMs = performance.now() - interactive;
                const start = performance.now(); avatar.click();
                let input;
                while (performance.now() - start < 5000) {
                    await frame();
                    input = document.querySelector('textarea.text-input-area');
                    if (input && document.querySelector('[data-chat-index]')) break;
                }
                if (!input || !document.querySelector('[data-chat-index]')) { state.interaction.failure = 2; return; }
                state.interaction.selectionMs = performance.now() - start;
                state.interaction.firstSelectionMs = performance.now() - interactive;
                const inputStart = performance.now();
                input.value = '__synthetic_input_sentinel__'; input.dispatchEvent(new Event('input', {bubbles: true}));
                await frame();
                const inputAccepted = input.value === '__synthetic_input_sentinel__';
                state.interaction.inputMs = performance.now() - inputStart;
                input.value = ''; input.dispatchEvent(new Event('input', {bubbles: true}));
                const scroll = document.querySelector('.default-chat-screen');
                if (!scroll) { state.interaction.failure = 3; return; }
                // Row mounting precedes layout. Include the bounded layout wait
                // in readiness instead of testing a still-empty scroll range.
                let stableFrames = 0, geometry = '';
                while (performance.now() - start < 5000 && stableFrames < 3) {
                    await frame();
                    const next = [scroll.scrollHeight, scroll.clientHeight, scroll.scrollTop].join('/');
                    stableFrames = next === geometry && scroll.scrollHeight > scroll.clientHeight ? stableFrames + 1 : 0;
                    geometry = next;
                }
                state.interaction.firstScrollReadyMs = performance.now() - interactive;
                if (stableFrames < 3) { state.interaction.failure = 4; return; }
                const before = scroll.scrollTop, scrollStart = performance.now();
                const maximum = scroll.scrollHeight - scroll.clientHeight;
                const reversed = getComputedStyle(scroll).flexDirection === 'column-reverse';
                scroll.dispatchEvent(new WheelEvent('wheel', {deltaY: reversed ? -100 : 100, bubbles: true}));
                scroll.scrollTop = reversed
                    ? before > -maximum + 1 ? Math.max(-maximum, before - 100) : Math.min(0, before + 100)
                    : before < maximum - 1 ? Math.min(maximum, before + 100) : Math.max(0, before - 100);
                state.interaction.scrollImmediateChanged = scroll.scrollTop !== before;
                await frame();
                state.interaction.scrollMs = performance.now() - scrollStart;
                state.interaction.inputAccepted = inputAccepted;
                state.interaction.scrollChanged = scroll.scrollTop !== before;
                state.interaction.success = inputAccepted && state.interaction.scrollChanged;
            })();
        }).observe({type: 'mark', buffered: true});
    }
    document.addEventListener('DOMContentLoaded', () => {
        const stages = {
            'Opening storage': 'storage', '저장소 여는 중': 'storage',
            'Preparing chat data': 'data', '대화 데이터 준비 중': 'data',
            'Preparing plugin compatibility data': 'compatibility', '플러그인 호환 데이터 준비 중': 'compatibility',
            'Preparing plugins': 'plugins', '플러그인 준비 중': 'plugins',
            'Preparing device sync': 'sync', '기기 동기화 준비 중': 'sync',
        };
        const observer = new MutationObserver(() => {
            const detail = document.querySelector('[role="status"] .loading-detail')?.textContent;
            const stage = stages[detail];
            if (stage && state.phases.at(-1)?.stage !== stage) {
                requestAnimationFrame(() => state.phases.push({stage, ms: performance.now()}));
            }
            if (document.querySelector('.loading-progress > [aria-live="off"]')) state.elapsedSeen = true;
        });
        observer.observe(document.documentElement, {subtree: true, childList: true, characterData: true});
        setTimeout(() => observer.disconnect(), 30000);
    });
    new PerformanceObserver(list => {
        for (const entry of list.getEntries()) state.longTasks.push({start: entry.startTime, ms: entry.duration});
    }).observe({type: 'longtask', buffered: true});
})()`
