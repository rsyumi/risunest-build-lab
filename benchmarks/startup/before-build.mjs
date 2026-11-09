import { readFile, writeFile } from 'node:fs/promises'
import { build } from 'vite'
import { startupInstrumentation } from './android-observation.mjs'
import { baselineFrontendPlugin, startupObservationPlugin, readObservationInputs, verifyObservationManifest } from './observe.mjs'
import { OBSERVATION_MANIFEST } from './metrics.mjs'

// Only this isolated benchmark build embeds the observer, before the app module executes.
const outDir = '.tmp/startup-benchmark-dist'
await build({
    mode: 'agent',
    build: { outDir },
    plugins: [baselineFrontendPlugin(), startupObservationPlugin(process.env.TAURI_ENV_PLATFORM)],
})
const file = `${outDir}/index.html`
const html = await readFile(file, 'utf8')
if (!html.includes('</head>')) throw new Error('Benchmark HTML has no head')
const observer = startupInstrumentation(
    process.env.TAURI_ENV_PLATFORM,
    process.env.STARTUP_DIAGNOSTIC_STAGES === 'true',
    verifyObservationManifest(
        JSON.parse(await readFile(`${outDir}/${OBSERVATION_MANIFEST}`, 'utf8')),
        await readObservationInputs(process.cwd(), process.env.TAURI_ENV_PLATFORM),
    ),
)
await writeFile(file, html.replace('</head>', `<script>${observer}</script></head>`))
