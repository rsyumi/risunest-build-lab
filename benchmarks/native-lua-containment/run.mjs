import { spawn } from 'node:child_process';
import { resolve } from 'node:path';
import { existsSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { classifyProbeResult } from './protocol.mjs';

export const cases = [
    'retention', 'finite-coroutine-resume', 'finite-coroutine-wrap', 'finite-coroutine-nested',
    'sync-callbacks', 'await', 'await-reject', 'await-cancel', 'await-deadline',
    'non-yieldable-await', 'nested-await', 'cancel-loop', 'cancel-callback', 'compile-recovery', 'allocation', 'error-recovery', 'finite-finalizer',
    'dispose-pending', 'churn', 'loop', 'coroutine', 'nested-coroutine', 'pcall', 'xpcall',
    'gsub', 'gsub-catch', 'sort', 'metamethod', 'error-handler', 'close-handler',
    'gc-handler', 'dispose-finalizer', 'native-pattern',
    'empty-rep', 'table-move-range', 'table-insert-range', 'table-remove-range',
    'table-concat-range', 'load-reader', 'load-binary-reader', 'finite-native',
    'finite-gc-modes', 'gc-generational', 'drop-reentrant', 'drop-native',
    'allocator-fault', 'retire-churn', 'callback-resources', 'idle-registration',
    'text-load-admission', 'gc-native-before', 'gc-native-after', 'drop-reset', 'allocator-init', 'hook-relocation', 'destroy-allocation',
    'conversion-alias', 'conversion-limits', 'conversion-parity', 'conversion-reverse', 'conversion-fanout',

];

export async function runCases(binary, selected, { prefixArguments = [], onResult = () => {} } = {}) {
    if (!binary || !existsSync(binary)) throw new Error('Pass the built containment executable path.');
    for (const name of selected) {
        if (!cases.includes(name)) throw new Error('Unknown synthetic case: ' + name);
    }
    const results = [];
    for (const name of selected) {
        const result = await new Promise((resolveResult) => {
            const child = spawn(resolve(binary), [...prefixArguments, name], { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
            let output = '';
            let errors = '';
            let watchdog = false;
            const timer = setTimeout(() => {
                watchdog = true;
                child.kill('SIGKILL');
            }, 2500);
            child.stdout.setEncoding('utf8').on('data', (chunk) => { output += chunk; });
            child.stderr.setEncoding('utf8').on('data', (chunk) => { errors += chunk; });
            child.once('error', (error) => {
                clearTimeout(timer);
                resolveResult({ case: name, status: 'launch-failed', detail: error.code });
            });
            child.once('close', (code, signal) => {
                clearTimeout(timer);
                resolveResult(classifyProbeResult(name, { output, errors, code, signal, watchdog }));
            });
        });
        results.push(result);
        onResult(result);
    }
    return results;
}

export async function main(args = process.argv.slice(2)) {
    const [binary, ...names] = args;
    const results = await runCases(binary, names.length ? names : cases, {
        onResult: (result) => console.log(JSON.stringify(result)),
    });
    const failed = results.filter((result) => result.status !== 'passed');
    console.log(JSON.stringify({
        summary: true,
        passed: results.length - failed.length,
        failed: failed.length,
        productionContainment: failed.length ? 'not-established' : 'only-selected-probes-passed',
    }));
    if (failed.length) process.exitCode = 1;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
    main().catch((error) => { console.error(error); process.exitCode = 1; });
}
