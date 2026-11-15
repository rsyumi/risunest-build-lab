import ts from 'typescript'
import { execFileSync } from 'node:child_process'
import path from 'node:path'
import { readFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { SDK_VERSION, IPC_BOUNDARY, SEMANTIC_BOUNDARY, OBSERVATION_MANIFEST,
    OBSERVER_INPUTS, IPC_COMMANDS, sanitizeObservationManifest } from './metrics.mjs'

const digest = value => createHash('sha256').update(value).digest('hex')
const sdkSuffix = '/@tauri-apps/api/core.js'
const storeSuffix = '/src/ts/storage/sqlitePersistentDataStore.ts'

export async function readObservationInputs(repository, platform, diagnosticStages = process.env.STARTUP_DIAGNOSTIC_STAGES === 'true') {
    if (!['windows', 'android', 'linux', 'macos', 'ios'].includes(platform))
        throw new Error('Explicit native startup platform required')
    if (typeof diagnosticStages !== 'boolean') throw new Error('Explicit observer diagnostic option required')
    const sdkDirectory = path.join(repository, 'node_modules/@tauri-apps/api')
    const version = JSON.parse(await readFile(path.join(sdkDirectory, 'package.json'), 'utf8')).version
    if (version !== SDK_VERSION) throw new Error('Unsupported startup SDK version')
    const observerInputs = {}
    for (const relative of OBSERVER_INPUTS) observerInputs[relative] = digest(await readFile(path.join(repository, relative)))
    const sdkSource = await readFile(path.join(sdkDirectory, 'core.js'), 'utf8')
    const storeFile = path.join(repository, 'src/ts/storage/sqlitePersistentDataStore.ts')
    const storeSource = await readFile(storeFile, 'utf8')
    const base = {
        schema: 1, boundary: IPC_BOUNDARY, semanticBoundary: SEMANTIC_BOUNDARY, platform,
        sourceRevision: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: repository, encoding: 'utf8' }).trim(),
        diagnosticStages,
        sdk: { version, sourceSha256: digest(sdkSource),
            transformedSha256: digest(instrumentSource(sdkSource, path.join(sdkDirectory, 'core.js'), platform, version)) },
        store: { sourceSha256: digest(storeSource),
            transformedSha256: digest(instrumentSource(storeSource, storeFile, platform)) },
        observerInputs, selectedCommands: [...IPC_COMMANDS],
    }
    return { ...base, inputFingerprint: digest(JSON.stringify(base)) }
}

export function verifyObservationManifest(manifest, inputs) {
    const verified = sanitizeObservationManifest(manifest)
    if (!verified || verified.inputFingerprint !== inputs.inputFingerprint
        || verified.platform !== inputs.platform || verified.sourceRevision !== inputs.sourceRevision
        || verified.diagnosticStages !== inputs.diagnosticStages
        || verified.sdk.sourceSha256 !== inputs.sdk.sourceSha256
        || verified.store.sourceSha256 !== inputs.store.sourceSha256
        || verified.sdk.transformedSha256 !== inputs.sdk.transformedSha256
        || verified.store.transformedSha256 !== inputs.store.transformedSha256
        || JSON.stringify(verified.observerInputs) !== JSON.stringify(inputs.observerInputs))
        throw new Error('Startup observer inputs differ from built artifact')
    return verified
}

// Build-only timing. No runtime source file or user-data accessor is installed.
const observed = {
    '/src/ts/storage/databasePreparation.ts': {
        prepareDatabaseForPersistence: 'prepare-import',
        prepareDatabaseForBootstrap: 'prepare-bootstrap',
        prepareDetachedDatabase: 'normalize',
    },
    '/src/ts/storage/saveCoordinatorHelpers.ts': { canonicalJson: 'canonical' },
    '/src/ts/storage/saveCoordinator.ts': {
        initialize: 'baseline',
        capture: 'capture',
        captureDatabase: 'capture-database',
        flushIterations: 'flush',
    },
    '/src/ts/bootstrap.ts': { loadData: 'bootstrap', cleanChunks: 'clean-chunks' },
    '/src/ts/globalApi.svelte.ts': { saveDb: 'save-observer' },
}

export function instrumentSource(source, id, platform, sdkVersion = SDK_VERSION) {
    const normalizedId = id.replaceAll('\\', '/').split('?')[0]
    if (normalizedId.endsWith(sdkSuffix) || normalizedId.endsWith(storeSuffix)) {
        const sdk = normalizedId.endsWith(sdkSuffix)
        if (sdk && sdkVersion !== SDK_VERSION) throw new Error('Unsupported startup SDK version')
        const file = ts.createSourceFile(id, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
        const bodies = []
        const visit = (node) => {
            if (sdk && ts.isFunctionDeclaration(node) && node.name?.text === 'invoke') {
                const statement = node.body?.statements[0]
                const call = statement && ts.isReturnStatement(statement) ? statement.expression : null
                if (!node.modifiers?.some(modifier => modifier.kind === ts.SyntaxKind.AsyncKeyword)
                    || node.parameters.map(parameter => parameter.getText(file).replace(/\s/g, '')).join(',') !== 'cmd,args={},options'
                    || node.body?.statements.length !== 1 || !call || !ts.isCallExpression(call)
                    || call.expression.getText(file).replace(/\s/g, '') !== 'window.__TAURI_INTERNALS__.invoke'
                    || call.arguments.map(argument => argument.getText(file)).join(',') !== 'cmd,args,options')
                    throw new Error('Unexpected startup SDK invoke AST')
                bodies.push(node.body)
            } else if (!sdk && ts.isMethodDeclaration(node) && node.name?.getText(file) === 'commit'
                && ts.isClassDeclaration(node.parent) && node.parent.name?.text === 'SqlitePersistentDataStore' && node.body) {
                bodies.push(node.body)
            }
            ts.forEachChild(node, visit)
        }
        visit(file)
        const exported = file.statements.some(statement => ts.isExportDeclaration(statement)
            && statement.exportClause && ts.isNamedExports(statement.exportClause)
            && statement.exportClause.elements.some(element => element.name.text === 'invoke' && !element.propertyName))
        if (bodies.length !== 1 || sdk && !exported) throw new Error('Missing or duplicate startup invocation boundary')
        const body = bodies[0]
        const wrapper = sdk ? '__startupObserveCall(cmd' : "__startupObserveOperation('store-commit'"
        return source.slice(0, body.getStart(file) + 1) + `return window.${wrapper}, () => {`
            + source.slice(body.getStart(file) + 1, body.end - 1) + '});' + source.slice(body.end - 1)
    }
    const suffix = Object.keys(observed).find((key) => normalizedId.endsWith(key))
    if (!suffix) return null
    const names =
        suffix.endsWith('databasePreparation.ts') &&
        !source.includes('function prepareDatabaseForBootstrap(')
            ? { prepareDatabaseForPersistence: 'prepare-import' }
            : observed[suffix]
    const file = ts.createSourceFile(id, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
    const edits = []
    const found = new Set()
    const wrap = (body, label) => {
        const variable = '__startupObservation'
        edits.push({
            at: body.getStart(file) + 1,
            text: `
            const ${variable} = performance.now();
            window.__startupBegin?.('${label}');
            let __startupSuccess = true;
            try {
        `,
        })
        edits.push({
            at: body.end - 1,
            text: `
            } catch (__startupError) { __startupSuccess = false; throw __startupError; }
            finally { window.__startupRecord?.('${label}', ${variable}, __startupSuccess); }
        `,
        })
    }
    const visit = (node) => {
        if (suffix.endsWith('bootstrap.ts') && ts.isIfStatement(node)) {
            let owner = node.parent
            while (owner && !ts.isFunctionDeclaration(owner)) owner = owner.parent
            const condition = node.expression.getText(file)
            const reason =
                owner?.name?.getText(file) !== 'cleanChunks'
                    ? 0
                    : condition === 'hasIncompletePersistentWorkingSet(db, workingSetResidency)'
                      ? 1
                      : condition === 'db.account?.useSync'
                        ? 2
                        : condition === 'db.coldstorage && !cleanColdStorage'
                          ? 3
                          : 0
            if (reason) {
                const statement = node.thenStatement
                edits.push({
                    at: statement.getStart(file),
                    text: `{ window.__startupMetrics.cleanChunksReason = ${reason}; `,
                })
                edits.push({ at: statement.end, text: ' }' })
            }
        }
        if (
            (ts.isFunctionDeclaration(node) || ts.isMethodDeclaration(node)) &&
            node.body &&
            node.name
        ) {
            const name = node.name.getText(file)
            if (names[name]) {
                found.add(name)
                wrap(node.body, names[name])
            }
        }
        if (
            suffix.endsWith('globalApi.svelte.ts') &&
            ts.isCallExpression(node) &&
            node.expression.getText(file) === '$effect'
        ) {
            let ancestor = node.parent
            while (ancestor && !ts.isFunctionDeclaration(ancestor)) ancestor = ancestor.parent
            if (ancestor?.name?.getText(file) === 'saveDb') {
                const callback = node.arguments[0]
                if (callback && ts.isArrowFunction(callback) && ts.isBlock(callback.body)) {
                    wrap(
                        callback.body,
                        callback.body.getText(file).includes('estimateRootBytes')
                            ? 'save-effect-root'
                            : 'save-effect-character',
                    )
                }
            }
        }
        ts.forEachChild(node, visit)
    }
    visit(file)
    for (const name of Object.keys(names)) {
        if (!found.has(name)) throw new Error(`Missing benchmark instrumentation point: ${name}`)
    }
    for (const edit of edits.sort((a, b) => b.at - a.at)) {
        source = source.slice(0, edit.at) + edit.text + source.slice(edit.at)
    }
    return source
}

export function startupObservationPlugin(platform, repository = process.cwd()) {
    let inputs
    const transformed = new Map()
    return {
        name: 'synthetic-startup-observation',
        enforce: 'pre',
        async buildStart() { inputs = await readObservationInputs(repository, platform) },
        transform(source, id) {
            const normalized = id.replaceAll('\\', '/').split('?')[0]
            const key = normalized.endsWith(sdkSuffix) ? 'sdk' : normalized.endsWith(storeSuffix) ? 'store' : null
            if (key && (!inputs || digest(source) !== inputs[key].sourceSha256 || transformed.has(key)))
                throw new Error('Unexpected or duplicate startup SDK/store module')
            const output = instrumentSource(source, id, platform, inputs?.sdk.version)
            if (key) transformed.set(key, digest(output))
            return output
        },
        buildEnd(error) {
            if (!error && (!inputs || transformed.size !== 2)) throw new Error('Missing startup SDK/store module')
        },
        generateBundle() {
            const manifest = {
                ...inputs,
                sdk: { ...inputs.sdk, transformedSha256: transformed.get('sdk') },
                store: { ...inputs.store, transformedSha256: transformed.get('store') },
            }
            verifyObservationManifest(manifest, inputs)
            this.emitFile({ type: 'asset', fileName: OBSERVATION_MANIFEST, source: JSON.stringify(manifest) })
        },
    }
}

export function baselineFrontendPlugin() {
    const files = new Set([
        'src/App.svelte',
        'src/ts/bootstrap.ts',
        'src/ts/stores.svelte.ts',
        'src/ts/storage/databasePreparation.ts',
        'src/ts/storage/persistentBootstrap.ts',
        'src/lib/UI/GUI/LoadingIndicator.svelte',
        'src/lang/en.ts',
        'src/lang/ko.ts',
    ])
    return {
        name: 'synthetic-startup-gc-only-baseline',
        enforce: 'pre',
        load(id) {
            if (process.env.STARTUP_BENCHMARK_VARIANT !== 'gc-only' || id.includes('?')) return null
            const relative = path.relative(process.cwd(), id).replaceAll('\\', '/')
            if (!files.has(relative)) return null
            // Only the original frontend is read. Native open is always the real modified command.
            return execFileSync(
                'git',
                [
                    '-c',
                    `safe.directory=${process.cwd().replaceAll('\\', '/')}`,
                    'show',
                    `dab32f2b585bbdd3b7e6d8ae4f8d52060350858e:${relative}`,
                ],
                { encoding: 'utf8' },
            )
        },
    }
}
