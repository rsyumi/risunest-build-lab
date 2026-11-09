function parseOutput(name, output) {
    let entered = false;
    let disposed = false;
    let complete = null;
    let phase = 'launch';
    const lines = output.split(/\r?\n/).filter((line) => line.trim().length);
    for (const [index, line] of lines.entries()) {
        let row;
        try { row = JSON.parse(line); }
        catch { throw new Error('invalid JSON record at line ' + (index + 1)); }
        if (!row || typeof row !== 'object' || Array.isArray(row)) throw new Error('record must be an object');
        if (row.case !== name) throw new Error('record case does not match requested case');
        if (complete) throw new Error('record after completion');
        if (!entered && row.phase !== 'entered') throw new Error('first record must be entered');
        const allowed = {
            entered: ['phase', 'case', 'lua', 'mlua'],
            dispose: ['phase', 'case'],
            diagnostic: ['phase', 'case', 'maximumLuaBytes'],
            complete: ['phase', 'case', 'status', 'elapsedMs'],
        }[row.phase];
        if (!allowed) throw new Error('unknown record phase');
        if (Object.keys(row).some((key) => !allowed.includes(key))) throw new Error('unexpected record field');
        if (row.phase === 'entered') {
            if (entered) throw new Error('duplicate entered record');
            if (row.lua !== '5.4.9' || row.mlua !== '0.12.2') throw new Error('unexpected runtime version');
            entered = true;
        } else if (row.phase === 'dispose') {
            if (disposed) throw new Error('duplicate dispose record');
            disposed = true;
        } else if (row.phase === 'diagnostic') {
            if (!Number.isSafeInteger(row.maximumLuaBytes) || row.maximumLuaBytes < 0) {
                throw new Error('invalid Lua memory diagnostic');
            }
        } else {
            if (row.status !== 'passed') throw new Error('completion must report passed');
            if (!Number.isFinite(row.elapsedMs) || row.elapsedMs < 0) throw new Error('invalid completion time');
            complete = row;
        }
        phase = row.phase;
    }
    if (!entered) throw new Error('missing entered record');
    return { phase, complete };
}

export function classifyProbeResult(name, { output, errors = '', code, signal = null, watchdog = false }) {
    const result = { case: name, status: 'failed', phase: 'launch', elapsedMs: null, exitCode: code, signal };
    try {
        const { phase, complete } = parseOutput(name, output);
        result.phase = phase;
        result.elapsedMs = complete?.elapsedMs ?? null;
        if (watchdog) result.status = 'containment-failed';
        else if (code !== 0 || signal !== null) result.protocolError = 'probe process did not exit successfully';
        else if (!complete) result.protocolError = 'missing complete record';
        else result.status = 'passed';
    } catch (error) {
        result.protocolError = error.message;
    }
    if (errors.trim()) result.diagnostic = errors.trim();
    return result;
}
