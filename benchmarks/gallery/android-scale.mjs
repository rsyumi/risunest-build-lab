import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import ts from 'typescript'
import { androidTarget, runnerOptions } from '../support/androidTarget.mjs'
import { connectSyntheticAndroid, delay } from '../startup/cdp.mjs'

const options = runnerOptions(process.argv.slice(2))
const android = androidTarget(options)
const port = Number(options['cdp-port'] ?? 19341)
const output = path.resolve(options.output ?? '.tmp/gallery-scale')
await mkdir(output, { recursive: true })
const adb = args => execFileSync(android.adb, android.args(args), {
    env: android.env, windowsHide: true, encoding: 'utf8', timeout: 15000, maxBuffer: 4 * 1024 * 1024,
})
android.assertAvd(adb(['emu', 'avd', 'name']))
assert.equal(adb(['shell', 'getprop', 'sys.boot_completed']).trim(), '1')
const packageName = 'io.github.rsyumi.risunest'
const appPid = adb(['shell', 'pidof', packageName]).trim()
assert.match(appPid, /^\d+$/)
adb(['forward', 'tcp:' + port, 'localabstract:webview_devtools_remote_' + appPid])
const client = await connectSyntheticAndroid(port, android)
const evaluate = (fn, args = []) => client.evaluate('(' + fn.toString() + ')(...' + JSON.stringify(args) + ')')
async function evaluateLong(fn, args) {
    await client.evaluate('window.__galleryOperation={done:false};window.__gallerySeedProgress=undefined;(' + fn.toString() + ')(...' + JSON.stringify(args) + ').then(value=>{window.__galleryOperation={done:true,value}},error=>{window.__galleryOperation={done:true,error:typeof error==="string"?error:JSON.stringify(error)}});true')
    for (;;) {
        const status = await client.evaluate('({done:window.__galleryOperation.done,progress:window.__gallerySeedProgress,error:window.__galleryOperation.error})')
        console.log(JSON.stringify(status))
        if (status.done) {
            assert.equal(status.error, undefined)
            return client.evaluate('window.__galleryOperation.value')
        }
        await delay(10000)
    }
}
const save = async (name, value) => writeFile(path.join(output, name + '.json'), JSON.stringify(value, null, 2))
function hostLoad() {
    if (process.platform !== 'win32') return null
    const command = `$os=Get-CimInstance Win32_OperatingSystem; $emulators=@(Get-Process qemu* -ErrorAction SilentlyContinue | Select-Object Id,ProcessName,CPU,PrivateMemorySize64); $compilers=@(Get-Process rustc,link,lld,lld-link,rust-lld,clang,cl,msbuild -ErrorAction SilentlyContinue | Select-Object Id,ProcessName,CPU); $workers=@(Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -ne ${process.pid} -and ( $_.Name -match '^(risunest_lib.*|native-baseline|native-final|native-mutated)[.]exe$' -or ($_.Name -match '^cargo[.]exe$' -and $_.CommandLine -match '\\btest\\b') -or ($_.Name -match '^node[.]exe$' -and $_.CommandLine -match 'vitest|tinypool|--test(?:\\s|$)|[\\\\/]tests[\\\\/]') ) } | Select-Object ProcessId,Name); @{freeCommitKiB=$os.FreeVirtualMemory;freePhysicalKiB=$os.FreePhysicalMemory;compilers=$compilers;workers=$workers;emulators=$emulators} | ConvertTo-Json -Compress -Depth 3`
    return JSON.parse(execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', command], {
        windowsHide: true, encoding: 'utf8', timeout: 15000,
    }))
}
function assertHostQuiet(host) {
    assert.equal(host?.compilers.length ?? 0, 0, 'Competing compiler prevents a valid measurement')
    assert.equal(host?.workers.length ?? 0, 0, 'Competing native or Node test worker prevents a valid measurement')
    if (host) assert.ok(host.freeCommitKiB >= 2 * 1024 * 1024, 'Less than 2 GiB host commit headroom prevents a valid measurement')
}
function memory() {
    const processes = adb(['shell', 'ps', '-A', '-o', 'PID,NAME']).split(/\r?\n/)
        .map(line => { const [pid, ...name] = line.trim().split(/\s+/); return { pid, processName: name.join(' ') } })
    const renderers = processes.filter(value => /webview.*sandboxed_process/.test(value.processName))
    assert.ok(renderers.length > 0, 'No sandboxed WebView renderer found; required renderer memory evidence is unavailable')
    const app = processes.find(value => value.pid === appPid)
    assert.ok(app, 'Synthetic application process disappeared')
    const selected = [app, ...renderers]
    return {
        rendererCandidateCount: renderers.length,
        rendererAttribution: 'All WebView sandboxed processes on the synthetic AVD; aggregate when more than one candidate exists',
        processes: selected.map(({ pid, processName }) => {
            const text = adb(['shell', 'dumpsys', 'meminfo', '--local', pid])
            const pss = /TOTAL PSS:\s*(\d+)/.exec(text)?.[1] ?? /^\s*TOTAL\s+(\d+)/m.exec(text)?.[1]
            const rss = /TOTAL RSS:\s*(\d+)/.exec(text)?.[1]
            const pssKiB = Number(pss), rssKiB = Number(rss)
            assert.ok(Number.isFinite(pssKiB) && pssKiB > 0, 'Missing or invalid PSS for ' + processName)
            assert.ok(Number.isFinite(rssKiB) && rssKiB > 0, 'Missing or invalid RSS for ' + processName)
            return { pid, processName, kind: pid === appPid ? 'app' : 'renderer', pssKiB, rssKiB }
        }),
    }
}
async function seedImages(poolSize, dimension) {
    const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(command, args)
    const sessionId = await invoke('asset_cas_job_begin', { kind: 'direct-asset-or-inlay-write' })
    const canvas = document.createElement('canvas')
    canvas.width = canvas.height = dimension
    const context = canvas.getContext('2d')
    const objects = []
    for (let index = 0; index < poolSize; index++) {
        window.__gallerySeedProgress = { stage: 'images', completed: index, total: poolSize }
        context.fillStyle = 'hsl(' + index * 137.508 + ' 60% 40%)'
        context.fillRect(0, 0, dimension, dimension)
        for (let stripe = 0; stripe < 16; stripe++) {
            context.fillStyle = 'hsl(' + (index * 137.508 + stripe * 13) + ' 75% ' + (25 + stripe * 3) + '%)'
            context.fillRect(stripe * dimension / 16, 0, dimension / 32, dimension)
        }
        context.fillStyle = '#fff'
        context.font = Math.floor(dimension / 12) + 'px sans-serif'
        context.fillText('Synthetic ' + index, dimension / 20, dimension / 2)
        const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'))
        const bytes = new Uint8Array(await blob.arrayBuffer())
        let prepared
        if (bytes.length <= 65536) {
            prepared = await invoke('asset_cas_job_prepare', { sessionId, role: 'direct-object', data: Array.from(bytes) })
        } else {
            const uploadId = crypto.randomUUID()
            await invoke('asset_cas_job_upload_open', { sessionId, uploadId, role: 'direct-object', totalBytes: bytes.length, binary: false })
            for (let offset = 0; offset < bytes.length; offset += 65536)
                await invoke('asset_cas_job_upload_chunk', { uploadId, offset, data: Array.from(bytes.subarray(offset, offset + 65536)) })
            prepared = await invoke('asset_cas_job_upload_finish', { uploadId })
        }
        objects.push({ hash: prepared.contentHash, size: prepared.byteSize })
    }
    await invoke('asset_cas_job_seal', { sessionId })
    canvas.width = canvas.height = 0
    return { sessionId, objects, dimension, uniqueBodies: new Set(objects.map(item => item.hash)).size }
}
async function seedAliases(fixture, count, assets, character, manifest) {
    const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(command, args)
    const manifestSession = await invoke('asset_cas_job_begin', { kind: 'card-or-module-content-import' })
    const uploadId = crypto.randomUUID()
    await invoke('asset_cas_job_upload_open', { sessionId: manifestSession, uploadId, role: 'owner-manifest', totalBytes: manifest.length, binary: false })
    for (let offset = 0; offset < manifest.length; offset += 65536)
        await invoke('asset_cas_job_upload_chunk', { uploadId, offset, data: manifest.slice(offset, offset + 65536) })
    const preparedManifest = await invoke('asset_cas_job_upload_finish', { uploadId })
    await invoke('asset_cas_job_seal', { sessionId: manifestSession })
    const read = await invoke('pds_read_root')
    const root = read.value
    root.language = 'ko'
    root.didFirstSetup = true
    root.characters = undefined
    root.galleryScaleSynthetic = true
    const scanCharacters = Math.floor(count / 100)
    root.characterOrder = ['gallery-scale-character', 'gallery-scale-archived-character', ...Array.from({ length: scanCharacters }, (_, index) => 'gallery-scale-scan-' + index)]
    const { stagingId } = await invoke('pds_replace_begin')
    await invoke('pds_replace_put_root', { stagingId, root })
    character.chaId = 'gallery-scale-character'
    character.name = 'Synthetic Gallery'
    character.image = ''
    character.additionalAssets = Array.from({ length: assets }, (_, index) => ['Synthetic ' + index, 'assets/gallery-scale-' + index + '.png', 'png'])
    character.chats = [
        { id: 'gallery-scale-chat', name: 'Synthetic Origin', note: '', localLore: [], fmIndex: 0,
          message: [{ role: 'char', data: 'Synthetic attachment {{inlay::gallery-scale-000000}}', chatId: 'gallery-scale-message', time: 1700000000000 }] },
        { id: 'gallery-scale-other-chat', name: 'Synthetic Other', message: [], note: '', localLore: [], fmIndex: 0 },
    ]
    character.chatPage = 0
    character.ccAssets = []
    character.emotionImages = []
    const archived = JSON.parse(JSON.stringify(character))
    archived.chaId = 'gallery-scale-archived-character'
    archived.name = 'Synthetic Archived'
    delete archived.additionalAssets
    archived.firstMessage = 'Synthetic archived attachment {{inlay::gallery-scale-000001}}'
    archived.chats = [{ id: 'gallery-scale-archived-chat', name: 'Synthetic Archived', note: '', localLore: [], fmIndex: 0,
        message: [{ role: 'char', data: 'Synthetic archived {{inlay::gallery-scale-000003}}', chatId: 'gallery-scale-archived-message', time: 1700000000000 }] }]
    await invoke('pds_replace_add_characters', { stagingId, characters: [character, archived] })
    let messageBytes = character.chats[0].message[0].data.length + archived.chats[0].message[0].data.length
    for (let index = 0; index < scanCharacters; index++) {
        window.__gallerySeedProgress = { stage: 'characters', completed: index, total: scanCharacters }
        const id = 'gallery-scale-scan-' + index
        const messages = Array.from({ length: 100 }, (_, messageIndex) => {
            const token = messageIndex % 20 === 0 ? '{{inlay::gallery-scale-' + String(2 + ((index * 100 + messageIndex) % (count - 4))).padStart(6, '0') + '}}' : ''
            const data = ('Synthetic scan ' + index + ' ' + messageIndex + ' ' + token).padEnd(512, 'x')
            messageBytes += data.length
            return { role: 'char', data, chatId: id + '-message-' + messageIndex, time: 1700000000000 + messageIndex }
        })
        const scanCharacter = { ...character, chaId: id, name: 'Synthetic Scan ' + index, additionalAssets: undefined, firstMessage: '', chats: [{ id: id + '-chat', name: 'Synthetic Scan', message: messages, note: '', localLore: [], fmIndex: 0 }] }
        await invoke('pds_replace_add_characters', { stagingId, characters: [scanCharacter] })
    }
    for (let start = 0; start < count + assets; start += 256) {
        window.__gallerySeedProgress = { stage: 'aliases', completed: start, total: count + assets }
        const aliases = []
        for (let index = start; index < Math.min(count + assets, start + 256); index++) {
            const asset = index >= count
            const n = asset ? index - count : index
            const object = fixture.objects[n % fixture.objects.length]
            aliases.push({
                key: asset ? 'assets/gallery-scale-' + n + '.png' : 'gallery-scale-' + String(n).padStart(6, '0'),
                objectHash: object.hash, kind: asset ? 'asset' : 'inlay',
                size: object.size, mime: 'image/png', name: 'synthetic-' + n + '.png', ext: 'png',
                ...(asset ? {} : { inlayType: 'image', width: fixture.dimension, height: fixture.dimension,
                    metadata: { createdAt: 1700000000000 + n, charId: n === 1 || n === 3 ? archived.chaId : character.chaId, chatId: n === 1 || n === 3 ? 'gallery-scale-archived-chat' : 'gallery-scale-chat' } }),
            })
        }
        await invoke('pds_replace_put_asset_aliases', { stagingId, aliases })
    }
    await invoke('pds_replace_put_asset_owner_heads', { stagingId, heads: [{
        owner: { kind: 'character-additional-assets', characterId: character.chaId },
        present: true, manifestHash: preparedManifest.contentHash, entryCount: assets,
    }] })
    const result = await invoke('pds_replace_commit', { stagingId, expectedRevision: read.revision })
    await invoke('asset_cas_job_release', { sessionId: manifestSession, outcome: 'committed' })
    if (count >= 100000) await invoke('asset_cas_job_release', { sessionId: fixture.sessionId, outcome: 'committed' })
    return { count, assets, revision: result.revision, uniqueBodies: fixture.uniqueBodies, dimension: fixture.dimension, scanCharacters, characters: scanCharacters + 2, conversations: scanCharacters + 3, messages: scanCharacters * 100 + 2, messageBytes }
}
async function metadataTimings(count) {
    const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(command, args)
    const results = []
    for (const sort of ['newest', 'oldest', 'largest']) {
        const query = { type: 'all', sort, usage: 'all', offset: 0, limit: 128 }
        const started = performance.now()
        const first = await invoke('pds_gallery_page', { query })
        const firstMs = performance.now() - started
        if (first.total !== count) throw new Error('Unexpected attachment count: ' + first.total)
        const later = []
        for (let index = 1; index <= 24; index++) {
            query.offset = Math.min(count - 128, Math.floor(count * index / 25))
            const before = performance.now()
            const page = await invoke('pds_gallery_page', { query })
            later.push({ offset: query.offset, ms: performance.now() - before, items: page.items.length })
        }
        results.push({ sort, total: first.total, firstMs, firstItems: first.items.length, later })
    }
    return results
}
function startScroll(total, dwellMs, requirePreviews) {
    const grid = document.querySelector('[data-gallery-grid]')
    if (!grid) throw new Error('Open exactly one synthetic gallery first')
    let scroller = grid.parentElement
    while (scroller && !/(auto|scroll)/.test(getComputedStyle(scroller).overflowY)) scroller = scroller.parentElement
    if (!scroller) throw new Error('Gallery scroller missing')
    const mediaRect = entry => (entry.querySelector('.media') ?? entry.querySelector('.cell') ?? entry).getBoundingClientRect()
    const state = window.__galleryScale = {
        total, dwellMs, requirePreviews, previewVerifiedWindows: 0, maxPreviewDimension: 0, done: false, error: null, windows: 0, maxCells: 0, maxImages: 0, maxIndex: -1,
        rafMetric: 'requestAnimationFrame callback interval; not displayed FPS', rafIntervalHistogramMs: Array(1001).fill(0), rafIntervalCount: 0, rafIntervalTotalMs: 0, rafIntervalMaxMs: 0,
        longTasks: { count: 0, totalMs: 0, maxMs: 0 }, coverageEnd: -1, coverageGaps: 0,
        pendingWindows: 0, failedWindows: [], settleHistogram50Ms: Array(101).fill(0), settleTotalMs: 0, settleMaxMs: 0, started: performance.now(),
    }
    const frame = now => {
        if (state.lastRafTimestamp !== undefined) {
            const ms = now - state.lastRafTimestamp
            state.rafIntervalHistogramMs[Math.min(1000, Math.round(ms))]++
            state.rafIntervalCount++
            state.rafIntervalTotalMs += ms
            state.rafIntervalMaxMs = Math.max(state.rafIntervalMaxMs, ms)
        }
        state.lastRafTimestamp = now
        if (!state.done) requestAnimationFrame(frame)
    }
    requestAnimationFrame(frame)
    const observer = new PerformanceObserver(list => {
        for (const entry of list.getEntries()) {
            state.longTasks.count++
            state.longTasks.totalMs += entry.duration
            state.longTasks.maxMs = Math.max(state.longTasks.maxMs, entry.duration)
        }
    })
    observer.observe({ type: 'longtask' })
    void (async () => {
        try {
            scroller.scrollTop = 0
            let previous = -1
            for (;;) {
                if (state.externalInterference) throw new Error('Competing host build/test worker or low commit headroom invalidated the scroll sample')
                const before = performance.now()
                const beforeScroll = scroller.scrollTop
                let loaded = false
                while (performance.now() - before < Math.max(5000, dwellMs)) {
                    await new Promise(resolve => requestAnimationFrame(resolve))
                    const cells = [...grid.querySelectorAll('[data-gallery-index]')]
                    const visibleEntries = [...grid.querySelectorAll('.entry')].filter(entry => {
                        const r = mediaRect(entry)
                        const s = scroller.getBoundingClientRect()
                        return r.bottom > Math.max(0, s.top) && r.top < Math.min(innerHeight, s.bottom)
                    })
                    loaded = cells.length > 0 && visibleEntries.length > 0 && visibleEntries.every(entry => {
                        const image = entry.querySelector('img')
                        return image?.complete && image.naturalWidth > 0
                    })
                    if (loaded && requirePreviews) {
                        for (const entry of visibleEntries) {
                            const image = entry.querySelector('img')
                            const thumbnail = new URL(image.currentSrc, image.baseURI).searchParams.get('thumbnail')
                            if (image.naturalWidth > 512 || image.naturalHeight > 512 || thumbnail !== 'gallery-v1') {
                                state.previewFailure = { index: entry.querySelector('[data-gallery-index]')?.dataset.galleryIndex, width: image.naturalWidth, height: image.naturalHeight, thumbnail }
                                throw new Error('Visible gallery image did not use the bounded native preview')
                            }
                            state.maxPreviewDimension = Math.max(state.maxPreviewDimension, image.naturalWidth, image.naturalHeight)
                        }
                    }
                    if (loaded && performance.now() - before >= dwellMs) {
                        try {
                            await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
                            break
                        } catch { loaded = false }
                    }
                }
                if (!loaded) {
                    state.pendingWindows++
                    const bounds = scroller.getBoundingClientRect()
                    const visible = [...grid.querySelectorAll('.entry')].filter(entry => {
                        const rect = mediaRect(entry)
                        return rect.bottom > Math.max(0, bounds.top) && rect.top < Math.min(innerHeight, bounds.bottom)
                    })
                    state.failedWindows.push({ at: Date.now(), elapsedLoadMs: performance.now() - before,
                        scrollTop: scroller.scrollTop, window: state.windows,
                        cells: visible.slice(0, 64).map(entry => {
                            const img = entry.querySelector('img')
                            const url = img?.currentSrc || img?.src || null
                            const resources = url ? performance.getEntriesByName(url).slice(-2).map(value => ({
                                startTime: value.startTime, duration: value.duration, responseStart: value.responseStart,
                                responseEnd: value.responseEnd, transferSize: value.transferSize,
                            })) : []
                            const rect = entry.getBoundingClientRect()
                            return { index: entry.dataset.galleryIndex ?? entry.querySelector('[data-gallery-index]')?.dataset.galleryIndex,
                                key: entry.dataset.galleryId ?? entry.querySelector('[data-gallery-id]')?.dataset.galleryId,
                                complete: img?.complete ?? false, width: img?.naturalWidth ?? 0, height: img?.naturalHeight ?? 0,
                                unavailable: !!entry.querySelector('.unavailable'), placeholder: !!entry.querySelector('.placeholder'),
                                metadataPlaceholder: !!entry.querySelector(':scope > .cell.placeholder'),
                                mediaPlaceholder: !!entry.querySelector('.media .placeholder'),
                                mediaTop: mediaRect(entry).top, mediaBottom: mediaRect(entry).bottom,
                                url, top: rect.top, bottom: rect.bottom, resources }
                        }) })
                    if (state.pendingWindows >= 3) throw new Error('Three visible windows failed to load within five seconds each')
                }
                const cells = [...grid.querySelectorAll('[data-gallery-index]')]
                const indices = cells.map(cell => Number(cell.dataset.galleryIndex))
                const maximum = Math.max(-1, ...indices)
                const settleMs = performance.now() - before
                state.settleHistogram50Ms[Math.min(100, Math.ceil(settleMs / 50))]++
                state.settleTotalMs += settleMs
                state.settleMaxMs = Math.max(state.settleMaxMs, settleMs)
                if (loaded && requirePreviews) state.previewVerifiedWindows++
                state.windows++
                state.maxCells = Math.max(state.maxCells, cells.length)
                state.maxImages = Math.max(state.maxImages, grid.querySelectorAll('img').length)
                state.maxIndex = Math.max(state.maxIndex, maximum)
                if (Math.min(...indices) > state.coverageEnd + 1) state.coverageGaps++
                state.coverageEnd = Math.max(state.coverageEnd, maximum)
                if (scroller.scrollTop >= scroller.scrollHeight - scroller.clientHeight - 2) break
                scroller.scrollTop += Math.max(1, Math.floor(scroller.clientHeight * 0.8))
                if (scroller.scrollTop === previous || scroller.scrollTop === beforeScroll) throw new Error('Scroll stopped before end')
                previous = beforeScroll
            }
            if (state.maxIndex !== total - 1) throw new Error('Final gallery item was not reached')
        } catch (error) { state.error = String(error) }
        finally { state.done = true; state.elapsedMs = performance.now() - state.started; observer.disconnect() }
    })()
    return { started: true, height: grid.clientHeight, viewport: scroller.clientHeight }
}
try {
    if (options.action === 'seed-images') {
        const fixture = await evaluateLong(seedImages, [Number(options.pool ?? 512), Number(options.dimension ?? 2048)])
        assert.equal(fixture.uniqueBodies, Number(options.pool ?? 512))
        fixture.totalBodyBytes = fixture.objects.reduce((sum, item) => sum + item.size, 0)
        await save('objects', fixture)
        console.log(JSON.stringify({ uniqueBodies: fixture.uniqueBodies, dimension: fixture.dimension }))
    } else if (options.action === 'seed-aliases') {
        const fixture = JSON.parse(await readFile(path.join(output, 'objects.json'), 'utf8'))
        const source = JSON.parse(await readFile(new URL('../../src-tauri/fixtures/persistent-fixture.json', import.meta.url), 'utf8'))
        const assets = Number(options.assets ?? 5000)
        const codecSource = await readFile(new URL('../../src/ts/storage/ownerManifestCodec.ts', import.meta.url), 'utf8')
        const compiled = ts.transpileModule(codecSource, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText
        const codecPath = path.join(output, 'ownerManifestCodec.mjs')
        await writeFile(codecPath, compiled)
        const { encodeOwnerManifest } = await import(pathToFileURL(codecPath).href)
        const manifest = encodeOwnerManifest(Array.from({ length: assets }, (_, index) => ({
            tuple: ['Synthetic ' + index, 'assets/gallery-scale-' + index + '.png', 'png'],
            payloadHash: new Uint8Array(Buffer.from(fixture.objects[index % fixture.objects.length].hash, 'hex')),
        })))
        const result = await evaluateLong(seedAliases, [fixture, Number(options.count ?? 100000), assets, source.characters[0], Array.from(manifest)])
        await save('fixture-' + result.count, result)
        console.log(JSON.stringify(result))
    } else if (options.action === 'archive-fixture') {
        const result = await client.evaluate("(async()=>{const invoke=(c,a={})=>window.__TAURI_INTERNALS__.invoke(c,a);const revision=await invoke('pds_read_revision');return invoke('pds_archive_character',{characterId:'gallery-scale-archived-character',expectedRevision:revision,operationId:crypto.randomUUID()})})()")
        await save('archive-fixture', result)
        console.log(JSON.stringify(result))
    } else if (options.action === 'memory') {
        const result = { at: new Date().toISOString(), memory: memory(), host: hostLoad() }
        await save('memory-preflight', result)
        console.log(JSON.stringify(result))
    } else if (options.action === 'metadata') {
        const count = Number(options.count ?? 100000)
        const beforeHost = hostLoad()
        assertHostQuiet(beforeHost)
        const result = await evaluate(metadataTimings, [count])
        const afterHost = hostLoad()
        await save('metadata-' + count, { count, memory: memory(), hostBefore: beforeHost, hostAfter: afterHost, results: result })
        assertHostQuiet(afterHost)
        console.log(JSON.stringify(result.map(({ sort, total, firstMs, later }) => ({ sort, total, firstMs, laterMedianMs: later.map(x => x.ms).sort((a,b) => a-b)[12] }))))
    } else if (options.action === 'scan') {
        const count = Number(options.count ?? 100000)
        const hostBefore = hostLoad()
        assertHostQuiet(hostBefore)
        const result = await evaluateLong(async cancel => {
            const invoke = (c, a = {}) => window.__TAURI_INTERNALS__.invoke(c, a)
            const revision = await invoke('pds_read_revision')
            const started = performance.now()
            const initial = await invoke('pds_gallery_scan_start', { expectedRevision: revision })
            const progress = []
            let status = initial
            let cancelResult
            do {
                status = await invoke('pds_gallery_scan_status')
                progress.push({ ms: performance.now() - started, state: status.state, phase: status.phase, completedConversations: status.completedConversations, totalConversations: status.totalConversations, completedItems: status.completedItems, totalItems: status.totalItems })
                if (cancel && status.state === 'running' && cancelResult === undefined) cancelResult = await invoke('pds_gallery_scan_cancel')
                window.__gallerySeedProgress = progress.at(-1)
                if (status.state === 'running') await new Promise(resolve => setTimeout(resolve, 50))
            } while (status.state === 'running')
            const elapsedMs = performance.now() - started
            const locations = []
            for (const key of ['gallery-scale-000000', 'gallery-scale-000001', 'gallery-scale-000003', 'gallery-scale-000009']) locations.push({ key, result: await invoke('pds_gallery_locations', { key }) })
            return { elapsedMs, cancelResult, status, progress, locations }
        }, [options.cancel === 'true'])
        const hostAfter = hostLoad()
        await save('scan-' + count + (options.cancel === 'true' ? '-cancel' : ''), { count, hostBefore, hostAfter, memory: memory(), ...result })
        assertHostQuiet(hostAfter)
        assert.equal(result.status.state, options.cancel === 'true' ? 'cancelled' : 'complete')
        if (options.cancel !== 'true') {
            for (const key of ['gallery-scale-000000', 'gallery-scale-000001', 'gallery-scale-000003']) {
                const found = result.locations.find(value => value.key === key).result
                assert.equal(found.state, 'used', key + ' must be used')
                assert.ok(found.locations.length > 0, key + ' must have a location')
                if (key !== 'gallery-scale-000000') assert.ok(found.locations.some(value => value.archived), key + ' must retain its archive location')
            }
            assert.equal(result.locations.find(value => value.key === 'gallery-scale-000009').result.state, 'unused')
        }
        console.log(JSON.stringify({ elapsedMs: result.elapsedMs, status: result.status.state, conversations: result.status.totalConversations, counts: result.status.counts }))
    } else if (options.action === 'scroll') {
        const label = options.label ?? 'gallery'
        assertHostQuiet(hostLoad())
        await client.call('Performance.enable')
        console.log(JSON.stringify(await evaluate(startScroll, [Number(options.count ?? 100000), Number(options.dwell ?? 50), options["require-previews"] === "true"])))
        const samples = []
        for (;;) {
            const state = await client.evaluate('({done:window.__galleryScale.done, windows:window.__galleryScale.windows,maxCells:window.__galleryScale.maxCells,maxIndex:window.__galleryScale.maxIndex,error:window.__galleryScale.error,rafIntervalCount:window.__galleryScale.rafIntervalCount,rafIntervalTotalMs:window.__galleryScale.rafIntervalTotalMs,longTaskMs:window.__galleryScale.longTasks.totalMs,pendingWindows:window.__galleryScale.pendingWindows,settleTotalMs:window.__galleryScale.settleTotalMs,previewVerifiedWindows:window.__galleryScale.previewVerifiedWindows,maxPreviewDimension:window.__galleryScale.maxPreviewDimension})')
            const metrics = await client.call('Performance.getMetrics')
            const browser = Object.fromEntries(metrics.metrics.filter(value => ['JSHeapUsedSize', 'JSHeapTotalSize', 'Nodes', 'Documents', 'TaskDuration'].includes(value.name)).map(value => [value.name, value.value]))
            const sample = { ...state, elapsedMs: Date.now(), memory: memory(), browser, host: hostLoad() }
            samples.push(sample)
            await save(label + '-samples', samples)
            console.log(JSON.stringify(sample))
            if (sample.host && (sample.host.compilers.length || sample.host.workers.length || sample.host.freeCommitKiB < 2 * 1024 * 1024)) await client.evaluate('window.__galleryScale.externalInterference=true;true')
            if (state.done) break
            await delay(10000)
        }
        const result = await client.evaluate('window.__galleryScale')
        await save(label + '-scroll', { result, samples })
        assert.equal(result.error, null)
        assert.equal(result.maxIndex, Number(options.count ?? 100000) - 1)
        assert.ok(result.maxCells < 512)
        assert.equal(result.pendingWindows, 0)
        assert.equal(result.coverageGaps, 0)
        if (options['require-previews'] === 'true') {
            assert.equal(result.previewVerifiedWindows, result.windows)
            assert.ok(result.maxPreviewDimension > 0 && result.maxPreviewDimension <= 512)
        }
    } else {
        throw new Error('--action=seed-images|seed-aliases|archive-fixture|memory|metadata|scan|scroll required')
    }
} finally { client.close() }
