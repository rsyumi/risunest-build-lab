import { instrumentation } from './cdp.mjs'
import { IPC_COMMANDS, sanitizeObservationManifest } from './metrics.mjs'

export function invocationObservation(manifest) {
    const verified = sanitizeObservationManifest(manifest)
    if (!verified) throw new Error('Invalid startup invocation observation manifest')
    return `(() => {
        const state = window.__startupMetrics;
        state.ipcObservation = ${JSON.stringify(verified)};
        state.observerFailure = null;
        const commands = new Set(${JSON.stringify(IPC_COMMANDS)});
        const safely = action => {
            try { return action(); } catch { state.observerFailure = 'recording-failed'; }
        };
        const observe = (command, invoke, operation) => {
            if (!operation && !commands.has(command)) return invoke();
            const start = performance.now();
            let entry;
            safely(() => {
                if (!operation) {
                    if (state.calls.length >= 10000) throw new Error('Observation limit');
                    entry = {command, start, ms: null, success: false, bytes: null};
                    state.calls.push(entry);
                    if (command === 'asset_cas_read_bytes') {
                        state.mediaInFlight++;
                        state.maxMediaInFlight = Math.max(state.maxMediaInFlight, state.mediaInFlight);
                    }
                }
                window.__startupBegin(command);
            });
            const finish = (success, result) => safely(() => {
                if (entry) {
                    entry.ms = performance.now() - start;
                    entry.success = success;
                    if (command === 'asset_cas_read_bytes') state.mediaInFlight--;
                    if (success && command === 'pds_open' && state.firstRevision === null
                        && Number.isSafeInteger(result?.revision)) state.firstRevision = result.revision;
                }
                window.__startupRecord(command, start, success);
            });
            let promise;
            try { promise = invoke(); } catch (error) { finish(false); throw error; }
            if (!promise || typeof promise.then !== 'function') { finish(true, promise); return promise; }
            try {
                promise.then(value => { finish(true, value); }, () => { finish(false); })
                    .catch(() => { state.observerFailure = 'recording-failed'; });
            } catch { state.observerFailure = 'recording-failed'; }
            return promise;
        };
        window.__startupObserveCall = (command, invoke) => observe(command, invoke, false);
        window.__startupObserveOperation = (stage, invoke) => observe(stage, invoke, true);
    })()`
}

export function startupInstrumentation(platform, diagnosticStages = false, manifest) {
    if (manifest?.platform !== platform || manifest.diagnosticStages !== diagnosticStages)
        throw new Error('Startup observer platform or diagnostic option mismatch')
    const diagnostics = `;(() => {
        const seen = new Set();
        for (const hook of ['__startupBegin', '__startupRecord']) {
            const original = window[hook];
            window[hook] = function(stage, ...args) {
                const key = hook + ':' + stage;
                if (!seen.has(key)) {
                    seen.add(key);
                    void window.__TAURI_INTERNALS__.invoke('native_log_error', {
                        message: 'M0-stage:' + key
                    }).catch(() => {});
                }
                return original.call(this, stage, ...args);
            };
        }
    })()`
    return (
        instrumentation +
        ';' +
        invocationObservation(manifest) +
        (diagnosticStages ? diagnostics : '')
    )
}
