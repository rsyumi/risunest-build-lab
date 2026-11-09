export const engineConfig = 'fuel+epoch;gc-null;wasm-gc-off;linear-memory-32MiB';
export const cases = {
    'cold-init-fuel': ['fuel'],
    retention: ['completed'],
    'nested-errors': ['completed'],
    'coroutine-yield': ['completed'],
    'entry-yield': ['completed'],
    'async-reentry': ['completed'],
    'await-reject-reuse': ['completed'],
    'invocation-state': ['completed'],
    'non-yieldable-await': ['non-yieldable-await-rejected'],
    'ordinary-error-recovery': ['completed'],
    'cancel-running': ['cancelled'],
    'cancel-pending': ['cancelled'],
    'deadline-pending': ['deadline'],
    'dispose-finalizer': ['store-dropped'],
    'nested-pcall': ['fuel', 'deadline'],
    'error-handler': ['fuel', 'deadline'],
    'reentry-loop': ['fuel', 'deadline'],
    'endless-reader': ['fuel', 'deadline'],
    compiler: ['fuel', 'deadline'],
    'native-pattern': ['fuel', 'deadline'],
    'empty-string-rep': ['fuel', 'deadline', 'completed-bounded'],
    'virtual-table': ['fuel', 'deadline'],
    'gc-finalizer': ['fuel', 'deadline'],
    'handles-effects': ['fuel', 'deadline'],
    'deadline-native-pattern': ['epoch-deadline'],
    'deadline-gc-finalizer': ['epoch-deadline'],
};

export function classify(name, expected, { output, errors = '', code, signal = null, watchdog = false, outputLimit = false }) {
    const result = { case: name, status: 'protocol-failed', phase: 'launch', exitCode: code, signal };
    let complete;
    try {
        if (!Object.hasOwn(cases, name)) throw new Error('unknown case');
        const rows = output.split(/\r?\n/).filter((line) => line.trim());
        for (const [index, line] of rows.entries()) {
            const row = JSON.parse(line);
            if (!row || typeof row !== 'object' || Array.isArray(row)) throw new Error('record must be an object');
            if (row.case !== name) throw new Error('case mismatch');
            if (complete) throw new Error('record after completion');
            const fields = row.phase === 'entered'
                ? ['phase', 'case', 'lua', 'wasmtime', 'backend', 'engineConfig', 'guestSha256']
                : row.phase === 'complete' ? ['phase', 'case', 'status', 'outcome', 'elapsedMs'] : [];
            if (Object.keys(row).length !== fields.length || fields.some((key) => !Object.hasOwn(row, key))) {
                throw new Error('unexpected or missing fields');
            }
            if (row.phase === 'entered') {
                if (index !== 0) throw new Error('duplicate or late entered record');
                if (row.lua !== '5.4.9' || row.wasmtime !== '48.0.7' || row.engineConfig !== engineConfig
                    || row.backend !== expected.backend || row.guestSha256 !== expected.guestSha256) {
                    throw new Error('runtime identity mismatch');
                }
            } else {
                if (index !== 1 || result.phase !== 'entered') throw new Error('complete before entered');
                const status = name === 'non-yieldable-await' ? 'limitation-confirmed' : 'passed';
                if (row.status !== status || !cases[name].includes(row.outcome)) throw new Error('unexpected completion result');
                if (!Number.isFinite(row.elapsedMs) || row.elapsedMs < 0) throw new Error('invalid elapsed time');
                complete = row;
            }
            result.phase = row.phase;
        }
        if (code !== 0 || signal !== null) result.status = 'probe-failed';
        else if (!complete) throw new Error('missing completion');
        else Object.assign(result, { status: complete.status, outcome: complete.outcome, elapsedMs: complete.elapsedMs });
    } catch (error) {
        result.protocolError = error.message;
    }
    if (watchdog) result.status = 'containment-failed';
    if (outputLimit) result.status = 'output-limit-failed';
    if (errors.trim()) result.diagnostic = errors.trim();
    return result;
}
