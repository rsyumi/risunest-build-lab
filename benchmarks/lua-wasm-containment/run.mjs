import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { cases, classify } from './protocol.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const digest = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');

export function verifyGuest(guest) {
    const receiptPath = guest.replace(/\.wasm$/, '.receipt.json');
    if (!guest.endsWith('.wasm') || !existsSync(guest) || !existsSync(receiptPath)) {
        throw new Error('prerequisite-missing: guest and build receipt required');
    }
    const receipt = JSON.parse(readFileSync(receiptPath, 'utf8'));
    const expected = {
        schema: 1,
        lua: '5.4.9',
        luaArchiveSha256: '2335b6c582a52654f94612bf10d2f4672805d05329aa6568b1d8cd9e5c6fb8e6',
        emscripten: '6.0.13',
        emsdkRelease: 'eb132757325583b787fe9414b09afea9442b72ad',
        bridgeSha256: digest(resolve(here, 'guest/bridge.c')),
        builderSha256: digest(resolve(here, 'guest/build.py')),
        wasmSha256: digest(guest),
    };
    for (const [key, value] of Object.entries(expected)) {
        if (receipt[key] !== value) throw new Error('prerequisite-mismatch: guest receipt ' + key);
    }
    for (const option of ['-sSUPPORT_LONGJMP=wasm', '-sWASM_LEGACY_EXCEPTIONS=0', '-sSTANDALONE_WASM=1', '--no-entry']) {
        if (!Array.isArray(receipt.options) || !receipt.options.includes(option)) throw new Error('guest build options mismatch');
    }
    return expected.wasmSha256;
}

export async function runCases(binary, guest, backend, selected, onResult = () => {}) {
    if (!existsSync(binary)) throw new Error('prerequisite-missing: compiled probe executable');
    if (!['cranelift', 'pulley'].includes(backend)) throw new Error('unsupported backend');
    for (const name of selected) if (!Object.hasOwn(cases, name)) throw new Error('unknown case: ' + name);
    const guestSha256 = verifyGuest(guest);
    const results = [];
    for (const name of selected) {
        const result = await new Promise((resolveResult) => {
            const child = spawn(resolve(binary), [resolve(guest), backend, name], {
                stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true,
            });
            let output = '';
            let errors = '';
            let watchdog = false;
            let outputLimit = false;
            let byteCount = 0;
            const timer = setTimeout(() => { watchdog = true; child.kill('SIGKILL'); }, 15_000);
            const append = (destination, chunk) => {
                byteCount += Buffer.byteLength(chunk);
                if (byteCount > 64 * 1024) {
                    if (!outputLimit) { outputLimit = true; child.kill('SIGKILL'); }
                    return;
                }
                if (destination === 'output') output += chunk;
                else errors += chunk;
            };
            child.stdout.setEncoding('utf8').on('data', (chunk) => append('output', chunk));
            child.stderr.setEncoding('utf8').on('data', (chunk) => append('errors', chunk));
            let launchError;
            child.once('error', (error) => { launchError = error.code; });
            child.once('close', (code, signal) => {
                clearTimeout(timer);
                resolveResult(launchError ? { case: name, status: 'launch-failed', detail: launchError }
                    : classify(name, { backend, guestSha256 }, { output, errors, code, signal, watchdog, outputLimit }));
            });
        });
        results.push(result);
        onResult(result);
    }
    return results;
}

export async function main(args = process.argv.slice(2)) {
    const [binary, guest, backend, ...names] = args;
    if (!binary || !guest || !backend) throw new Error('usage: node run.mjs executable guest.wasm cranelift|pulley [case ...]');
    const results = await runCases(binary, guest, backend, names.length ? names : Object.keys(cases),
        (result) => console.log(JSON.stringify(result)));
    const failed = results.filter((result) => !['passed', 'limitation-confirmed'].includes(result.status)).length;
    console.log(JSON.stringify({ summary: true, backend, binarySha256: digest(binary), guestSha256: digest(guest),
        passed: results.filter((result) => result.status === 'passed').length,
        limitations: results.filter((result) => result.status === 'limitation-confirmed').length, failed,
        productionContainment: 'not-established', evidence: 'only-selected-synthetic-probes' }));
    if (failed) process.exitCode = 1;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
    main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
