<script lang="ts">
    import { onDestroy, tick, untrack } from 'svelte'
    import { XIcon, DownloadIcon, PlusIcon, SearchIcon } from '@lucide/svelte'
    import { language } from 'src/lang'
    import { DBState } from 'src/ts/stores.svelte'
    import { saveAsset, getFileImageSource } from 'src/ts/globalApi.svelte'
    import { isTauri } from 'src/ts/platform'
    import { gallerySize } from 'src/ts/gallery/preferences'
    import { selectMultipleFile, selectSingleFile } from 'src/ts/util'
    import { imageAliases, selectedImageFormat, stripImageExtension } from 'src/ts/media/imageFormats'
    import { selectCharImg } from 'src/ts/characters'
    import { alertActionConfirm, alertError, alertToast } from 'src/ts/alert'
    import { commitPreparedUnitIntent, mutatePersistentCharacterDetail, readPersistentCharacterDetail } from 'src/ts/storage/persistentDataRuntime.svelte'
    import type { PersistentRevisionLease } from 'src/ts/storage/persistentDataStore'
    import { releasePersistentRevisionLease } from 'src/ts/storage/persistentRecordIterator'
    import { getPersistentDataStore } from 'src/ts/storage/persistentDataStoreFactory'
    import { modalNavigation } from 'src/ts/ui/modalNavigation'
    import { galleryText, galleryFormat } from 'src/ts/gallery/strings'
    import { galleryBytes, type GalleryItem, type GallerySize } from 'src/ts/gallery/types'
    import { downloadGallery } from 'src/ts/gallery/galleryDownloads'
    import {
        prepareGalleryModuleMutation, readPinnedAssetOwner, assetGalleryItems, assetGalleryPage, assetSectionCount, assetExtension, assertAssetSnapshots, addGalleryAssets,
        deleteGalleryAssets, selectionAfterAssetDelete, renameGalleryAsset, replaceGalleryAsset, setGalleryCharacterIcon,
        setGalleryPromptExcluded, validateAssetName, AssetNameError, StaleAssetSelectionError,
        type AssetOwner, type AssetOwnerData, type AssetSection, type AssetGalleryItem,
    } from 'src/ts/gallery/assetGallery'
    import GalleryGrid from './GalleryGrid.svelte'
    import GalleryViewer from './GalleryViewer.svelte'
    import GalleryToolbar from './GalleryToolbar.svelte'
    import Button from 'src/lib/UI/GUI/Button.svelte'
    import SettingProgress from 'src/lib/Setting/RisuNest/SettingProgress.svelte'
    import SettingNotice from 'src/lib/Setting/RisuNest/SettingNotice.svelte'
    import './gallery.css'

    let { owner, initialSection = 'assets', onclose }: { owner: AssetOwner; initialSection?: AssetSection; onclose: () => void } = $props()
    const text = $derived(galleryText())
    let section = $state<AssetSection>(untrack(() => initialSection))
    let tabNavigation = $state<HTMLElement>()
    let search = $state('')
    let filter = $state<GalleryItem['type'] | 'all'>('all')
    const size = $derived($gallerySize)
    let selecting = $state(false)
    let selections = $state(new Map<string, AssetGalleryItem>())
    const selected = $derived(new Set(selections.keys()))
    let lastSelectedIndex = -1
    let offset = $state(0)
    let limit = $state(80)
    let grid: GalleryGrid
    let viewer = $state<AssetGalleryItem | null>(null)
    let viewerIndex = $state(0)
    let opener: { item: AssetGalleryItem; index: number } | null = null
    let viewerMetadata = $state<Partial<GalleryItem>>({})
    let busy = $state(false)
    let renameTarget = $state<AssetGalleryItem | null>(null)
    let renameValue = $state('')
    let renameError = $state('')
    let notice = $state('')
    let progressTitle = $state('')
    let progress = $state<{ done: number; total: number } | null>(null)
    let downloadAbort: AbortController | undefined
    const allExtensions = [...imageAliases, 'svg', 'bmp', 'mp4', 'webm', 'mov', 'mp3', 'wav', 'ogg', 'flac', 'm4a', 'ttf', 'otf', 'css', 'woff', 'woff2']
    const currentOwner = $derived(owner.kind === 'character'
        ? DBState.db.characters.find(value => value.chaId === owner.id)
        : DBState.db.modules.find(value => value.id === owner.id))
    const data = $derived((currentOwner ?? {}) as AssetOwnerData)
    const ownerName = $derived(currentOwner?.name ?? '')
    const sections: AssetSection[] = ['icons', 'emotions', 'assets']
    const filters = ['all', 'image', 'video', 'audio', 'other'] as const
    const normalizedSearch = $derived(search.trim().toLocaleLowerCase())
    function matches(entry: AssetGalleryItem) {
        return (filter === 'all' || entry.type === filter) && (!normalizedSearch || entry.name.toLocaleLowerCase().includes(normalizedSearch))
    }
    const counts = $derived({
        icons: assetSectionCount(owner, data, 'icons'),
        emotions: assetSectionCount(owner, data, 'emotions'),
        assets: assetSectionCount(owner, data, 'assets'),
    })
    const typeCounts = $derived.by(() => {
        const result = { all: 0, image: 0, video: 0, audio: 0, other: 0 }
        for (const entry of assetGalleryItems(owner, data, section)) {
            if (!normalizedSearch || entry.name.toLocaleLowerCase().includes(normalizedSearch)) { result.all++; result[entry.type]++ }
        }
        return result
    })
    const total = $derived(typeCounts[filter])
    const items = $derived(assetGalleryPage(owner, data, section, normalizedSearch, filter, offset, limit))
    const listing = $derived({ counts, typeCounts, total, items })
    $effect(() => {
        normalizedSearch
        filter
        untrack(() => { clear(); offset = 0; void grid?.scrollToIndex(0).catch(report) })
    })
    function at(index: number): AssetGalleryItem | undefined {
        if (index < 0 || index >= total) return
        return assetGalleryPage(owner, data, section, normalizedSearch, filter, index, 1).get(index)
    }
    $effect(() => {
        const navigation = tabNavigation
        section
        if (!navigation) return
        let active = true
        void tick().then(() => {
            if (!active) return
            const tab = navigation.querySelector<HTMLElement>('[aria-current="page"]')
            if (!tab) return
            const viewport = navigation.getBoundingClientRect()
            const bounds = tab.getBoundingClientRect()
            if (bounds.left < viewport.left) navigation.scrollLeft += bounds.left - viewport.left
            else if (bounds.right > viewport.right) navigation.scrollLeft += bounds.right - viewport.right
        })
        return () => { active = false }
    })
    $effect(() => {
        typeCounts
        filter
        untrack(() => {
            const kept = new Map<string, AssetGalleryItem>()
            for (const [id, snapshot] of selections) {
                try { assertAssetSnapshots(owner, data, [snapshot]); if (matches(snapshot)) kept.set(id, snapshot) } catch {}
            }
            if (kept.size !== selections.size) selections = kept
            if (viewer) {
                let found: AssetGalleryItem | undefined
                let foundIndex = 0
                let relocated: { entry: AssetGalleryItem; index: number } | undefined
                for (const entry of assetGalleryItems(owner, data, section)) if (matches(entry)) {
                    if (entry.name === viewer.name && entry.key === viewer.key) {
                        if (entry.id === viewer.id) { found = entry; break }
                        if (!relocated || Math.abs(foundIndex - viewerIndex) < Math.abs(relocated.index - viewerIndex)) relocated = { entry, index: foundIndex }
                    }
                    foundIndex++
                }
                if (found) { viewer = found; viewerIndex = foundIndex }
                else if (relocated) { viewer = relocated.entry; viewerIndex = relocated.index }
                else { viewerIndex = Math.min(viewerIndex, listing.total - 1); viewer = at(viewerIndex) ?? null }
            }
        })
    })
    $effect(() => {
        const target = viewer
        viewerMetadata = {}
        if (!target) return
        let active = true
        void Promise.all([
            getPersistentDataStore().readAssetAlias({ kind: 'asset', key: target.key }),
            target.type === 'image' ? getFileImageSource(target.key) : Promise.resolve(null),
        ]).then(([alias, image]) => {
            if (active) viewerMetadata = {
                ...(alias ? { size: alias.value.size } : {}),
                ...(image ? { width: image.width, height: image.height, size: image.metadata.size } : {}),
            }
        }).catch(() => {})
        return () => { active = false }
    })
    onDestroy(() => downloadAbort?.abort())
    function changeSection(value: AssetSection) { section = value; filter = 'all'; search = ''; clear(); offset = 0; void grid?.scrollToIndex(0).catch(report) }
    function clear() { selections = new Map(); selecting = false; lastSelectedIndex = -1 }
    function toggle(item: GalleryItem, index: number, range: boolean) {
        const next = new Map(selections)
        if (range && lastSelectedIndex >= 0) {
            let position = 0
            for (const entry of assetGalleryItems(owner, data, section)) if (matches(entry)) {
                if (position >= Math.min(index, lastSelectedIndex) && position <= Math.max(index, lastSelectedIndex)) next.set(entry.id, entry)
                position++
            }
        } else if (next.has(item.id)) next.delete(item.id)
        else next.set(item.id, item as AssetGalleryItem)
        selections = next; lastSelectedIndex = index
    }
    function selectAll() { selections = new Map(Array.from(assetGalleryItems(owner, data, section)).filter(matches).map(entry => [entry.id, entry])) }
    function setSize(value: GallerySize) { gallerySize.set(value) }
    async function closeViewer() {
        viewer = null
        let focusIndex = opener?.index ?? viewerIndex
        let index = 0
        let relocated: number | undefined
        if (opener) for (const entry of assetGalleryItems(owner, data, section)) if (matches(entry)) {
            if (entry.name === opener.item.name && entry.key === opener.item.key) {
                if (entry.id === opener.item.id) { relocated = index; break }
                if (relocated === undefined || Math.abs(index - opener.index) < Math.abs(relocated - opener.index)) relocated = index
            }
            index++
        }
        if (relocated !== undefined) focusIndex = relocated
        await tick()
        await grid?.focusItem(Math.max(0, Math.min(focusIndex, listing.total - 1)))
        opener = null
    }
    async function report(error: unknown) {
        if (error instanceof StaleAssetSelectionError) { clear(); if (owner.kind === 'character') await readPersistentCharacterDetail(owner.id, 'gallery-stale-selection'); alertToast(text.staleAsset) }
        else alertError(String(error))
    }
    async function mutate(action: string, change: (value: AssetOwnerData) => void) {
        if (owner.kind === 'character') {
            const found = await mutatePersistentCharacterDetail(owner.id, `gallery-${action}`, ({ character }) => change(character))
            if (!found) throw new StaleAssetSelectionError()
        } else {
            await commitPreparedUnitIntent(`gallery-${action}`, reader => prepareGalleryModuleMutation(reader, owner.id, change))
        }
    }
    async function action(operation: () => Promise<void>) {
        if (busy) return
        busy = true
        try { await operation() } catch (error) { await report(error) } finally { busy = false }
    }
    // An identified image is stored and declared under its canonical extension, like the other asset selectors.
    async function storeFile(file: { name: string; data: Uint8Array }) {
        const format = selectedImageFormat(file.data, file.name)
        const ext = format?.ext ?? assetExtension(file.name)
        return { key: await saveAsset(file.data, '', format ? `${stripImageExtension(file.name)}.${ext}` : file.name), ext }
    }
    async function add() {
        const targetSection = section
        await action(async () => {
            const files = targetSection === 'assets' ? await selectMultipleFile(allExtensions)
                : await selectMultipleFile(imageAliases, { image: true })
            if (!files?.length) return
            const prepared: { name: string; key: string; ext: string }[] = []
            for (const file of files) prepared.push({ name: file.name, ...await storeFile(file) })
            await mutate('add-assets', value => addGalleryAssets(owner, value, targetSection, prepared))
        })
    }
    async function remove(entries: AssetGalleryItem[]) {
        const removable = entries.filter(entry => !entry.portrait)
        if (!removable.length) return
        await action(async () => {
            if (!await alertActionConfirm({ title: text.deleteAssetsTitle, description: galleryFormat(text.deleteAssets, removable.length), cancelLabel: text.cancel, actionLabel: text.remove })) return
            const previousSelection = [...selections.values()]
            await mutate('delete-assets', value => deleteGalleryAssets(owner, value, removable))
            selections = selectionAfterAssetDelete(owner, data, previousSelection, removable)
            lastSelectedIndex = -1
        })
    }
    async function replace(target: AssetGalleryItem) {
        await action(async () => {
            if (target.portrait) {
                assertAssetSnapshots(owner, data, [target])
                const index = DBState.db.characters.findIndex(value => value.chaId === owner.id)
                if (index >= 0) await selectCharImg(index, value => assertAssetSnapshots(owner, value, [target]))
                return
            }
            const file = target.section === 'assets' ? await selectSingleFile(allExtensions)
                : await selectSingleFile(imageAliases, { image: true })
            if (!file) return
            const { key, ext } = await storeFile(file)
            await mutate('replace-asset', value => replaceGalleryAsset(owner, value, target, key, ext))
        })
    }
    function startRename(target: AssetGalleryItem) { renameTarget = target; renameValue = target.name; renameError = '' }
    async function rename() {
        const target = renameTarget
        if (!target || busy) return
        try { validateAssetName(owner, data, target.section, renameValue, target.position) }
        catch (error) { if (error instanceof AssetNameError) renameError = error.reason === 'empty' ? text.nameEmpty : text.nameDuplicate; return }
        busy = true
        try { await mutate('rename-asset', value => renameGalleryAsset(owner, value, target, renameValue)); renameTarget = null }
        catch (error) { if (error instanceof AssetNameError) renameError = error.reason === 'empty' ? text.nameEmpty : text.nameDuplicate; else { renameTarget = null; await report(error) } }
        finally { busy = false }
    }
    async function download(entries: AssetGalleryItem[], archive: boolean, wholeOwner = false) {
        await action(async () => {
            if (!wholeOwner) assertAssetSnapshots(owner, data, entries)
            notice = ''
            progressTitle = archive ? text.zipTitle : text.download
            downloadAbort = new AbortController()
            let exportLease: PersistentRevisionLease | undefined
            const captureEntries = async (revision: number) => {
                if (exportLease) { await releasePersistentRevisionLease(exportLease); exportLease = undefined }
                exportLease = await getPersistentDataStore().acquireRevision(revision)
                const current = await readPinnedAssetOwner(exportLease, owner)
                if (wholeOwner) entries = sections.flatMap(value => Array.from(assetGalleryItems(owner, current, value)))
                else assertAssetSnapshots(owner, current, entries)
                return entries.map(entry => ({ key: entry.key, name: entry.portrait ? text.currentIcon : entry.name, section: text[entry.section] }))
            }
            try {
                const result = await downloadGallery({ kind: 'asset', archive, suggestedName: archive ? `${galleryFormat(text.assetTitle, ownerName)}.zip` : `${entries[0]?.name || text.currentIcon}.${entries[0]?.ext}`, item: entries[0] ? { ...entries[0], name: entries[0].portrait ? text.currentIcon : entries[0].name } : undefined, captureEntries }, { signal: downloadAbort.signal, onProgress: value => progress = value })
                if (!result.cancelled) {
                    if (archive) notice = result.skipped ? galleryFormat(text.zipSkipped, result.skipped) : text.zipSaved
                    else alertToast(text.fileSaved)
                }
            } finally {
                progress = null; downloadAbort = undefined
                if (exportLease) await releasePersistentRevisionLease(exportLease)
            }
        })
    }
</script>

<div class="asset-overlay" role="dialog" aria-modal="true" aria-label={galleryFormat(text.assetTitle, ownerName)} tabindex="-1" use:modalNavigation={{ close: onclose }}>
    <div class="asset-panel">
        <header class="asset-header"><h2>{galleryFormat(text.assetTitle, ownerName)}{#if owner.kind === 'module'}<small>{text.assets} {listing.counts.assets}</small>{/if}</h2><button class="asset-close" aria-label={text.close} onclick={onclose}><XIcon size={20} /></button></header>
        {#if owner.kind === 'character'}
            <div class="asset-tabs"><nav bind:this={tabNavigation} class="g-tabs" aria-label={text.gallery}>{#each sections as value}<button class:active={section === value} aria-current={section === value ? 'page' : undefined} onclick={() => changeSection(value)}>{text[value]} <span class="count">{listing.counts[value]}</span></button>{/each}</nav></div>
        {/if}
        <div class="asset-body">
            {#if selections.size}
                <GalleryToolbar selectedCount={selections.size} total={listing.total} onselectall={selectAll} onclear={clear}>
                    {#if isTauri}<button class="g-button small" disabled={busy} onclick={() => download([...selections.values()], true)}><DownloadIcon size={14}/>{text.download}</button>{/if}
                    <button class="g-button small danger" disabled={busy || ![...selections.values()].some(entry => !entry.portrait)} onclick={() => remove([...selections.values()])}>{text.remove}</button>
                </GalleryToolbar>
            {:else}
                <div class="g-toolbar" data-gallery-toolbar>
                    <label class="g-search"><SearchIcon size={16}/><input aria-label={text.search} placeholder={text.search} bind:value={search}/></label>
                    {#if section === 'assets'}<div class="g-row">{#each filters as value}<button class="g-chip" class:active={filter === value} onclick={() => { filter = value }}>{text[value]} <span>{listing.typeCounts[value]}</span></button>{/each}</div>{/if}
                    <span class="g-grow"></span><div class="g-row">
                        <div class="g-segment"><button class:active={size === 'normal'} onclick={() => setSize('normal')}>{text.normal}</button><button class:active={size === 'large'} onclick={() => setSize('large')}>{text.large}</button></div>
                        <button class="g-button small" class:active={selecting} onclick={() => selecting = !selecting}>{selecting ? text.cancel : text.select}</button>
                        <button class="g-button small primary" disabled={busy} onclick={add}><PlusIcon size={14}/>{text.add}</button>
                        {#if isTauri}<button class="g-button small" disabled={busy} onclick={() => download([], true, true)}><DownloadIcon size={14}/>{text.downloadAll}</button>{/if}
                    </div>
                </div>
            {/if}
            {#if notice || progress}<div class="g-notices">{#if notice}<SettingNotice tone="info" text={notice}/>{/if}{#if progress}<SettingProgress label={progressTitle} detail={galleryFormat(text.zipProgress, progress.done, progress.total)} fraction={progress.total > 0 ? progress.done / progress.total : null}>{#snippet actions()}<button class="g-button small" onclick={() => downloadAbort?.abort()}>{text.cancel}</button>{/snippet}</SettingProgress>{/if}</div>{/if}
            <GalleryGrid bind:this={grid} total={listing.total} items={listing.items} {size} captions {selected} {selecting} onwindow={(nextOffset, nextLimit) => { offset = nextOffset; limit = nextLimit }} onopen={(item, index) => { opener = { item: item as AssetGalleryItem, index }; viewer = item as AssetGalleryItem; viewerIndex = index }} ontoggle={toggle}/>
            {#if !listing.total}<p class="asset-empty">{language.noData}</p>{/if}
        </div>
    </div>
</div>
{#if viewer}
    {@const target = viewer}
    <GalleryViewer item={{ ...target, ...viewerMetadata, name: target.portrait ? text.currentIcon : target.name }} index={viewerIndex} total={listing.total} previous={at(viewerIndex - 1)} next={at(viewerIndex + 1)} onclose={closeViewer} onnavigate={(index) => { viewerIndex = index; viewer = at(index) ?? null }}>
        {#snippet info()}
            <dl class="asset-info">
                <dt>{text.name}</dt><dd>{target.portrait ? text.currentIcon : target.name}{#if !target.portrait}<button class="g-button small quiet" disabled={busy} onclick={() => startRename(target)}>{text.rename}</button>{/if}</dd>
                <dt>{text.extension}</dt><dd>{target.ext}</dd>
                {#if viewerMetadata.width && viewerMetadata.height}<dt>{text.resolution}</dt><dd>{viewerMetadata.width} × {viewerMetadata.height}</dd>{/if}
                {#if viewerMetadata.size !== undefined}<dt>{text.size}</dt><dd>{galleryBytes(viewerMetadata.size)}</dd>{/if}
            </dl>
            {#if owner.kind === 'character' && target.section === 'assets' && data.prebuiltAssetCommand}<label class="asset-exclude"><input type="checkbox" disabled={busy} checked={data.prebuiltAssetExclude?.includes(target.key) ?? false} onchange={(event) => { const checked = event.currentTarget.checked; void action(() => mutate('exclude-asset', value => setGalleryPromptExcluded(owner, value, target, checked))) }}/>{text.promptExclude}</label>{/if}
        {/snippet}
        {#snippet actions()}
            {#if target.section === 'icons' && !target.portrait}<button class="g-button small" disabled={busy} onclick={() => action(() => mutate('set-icon', value => setGalleryCharacterIcon(owner, value, target)))}>{text.setIcon}</button>{/if}
            <button class="g-button small" disabled={busy} onclick={() => replace(target)}>{text.replace}</button>
            <button class="g-button small" disabled={busy} onclick={() => download([target], false)}>{text.download}</button>
            {#if !target.portrait}<button class="g-button small danger" disabled={busy} onclick={() => remove([target])}>{text.remove}</button>{/if}
        {/snippet}
    </GalleryViewer>
{/if}
{#if renameTarget}
    <div class="rename-overlay" role="dialog" aria-modal="true" aria-labelledby="asset-rename-title" tabindex="-1" use:modalNavigation={{ close: () => renameTarget = null }}>
        <form class="rename-dialog" onsubmit={(event) => { event.preventDefault(); void rename() }}>
            <h3 id="asset-rename-title">{text.rename}</h3>
            <div class="rename-field"><label for="asset-rename-value">{text.name}</label>
            <input id="asset-rename-value" value={renameValue} oninput={(event) => { renameValue = event.currentTarget.value; renameError = '' }} aria-invalid={!!renameError} aria-describedby="asset-rename-help asset-rename-error"/>
            <span id="asset-rename-error" class="rename-error" role="alert">{renameError}</span>
            <p id="asset-rename-help">{text.renameHelp}</p></div>
            <div class="rename-buttons"><Button styled="outlined" onclick={(event) => { event.preventDefault(); renameTarget = null }}>{text.cancel}</Button><Button disabled={busy} onclick={(event) => { event.preventDefault(); void rename() }}>{text.change}</Button></div>
        </form>
    </div>
{/if}

<style>
    .asset-overlay { position:fixed; inset:0; z-index:130; background:rgba(0,0,0,.6); display:flex; align-items:center; justify-content:center; padding:1.25rem; color:var(--risu-theme-textcolor); }
    .asset-panel { width:min(72rem,100%); height:90dvh; display:flex; flex-direction:column; overflow:hidden; background:var(--risu-theme-bgcolor); border:1px solid var(--risu-theme-darkborderc); border-radius:.75rem; }
    .asset-header { display:flex; align-items:center; justify-content:space-between; gap:.75rem; padding:.875rem 1rem 0; flex:none; }
    .asset-header h2 { flex:1; min-width:0; font-size:1.125rem; font-weight:700; margin:0; overflow-wrap:anywhere; }
    .asset-header small { margin-left:.375rem; color:var(--risu-theme-textcolor2); font-size:13px; font-weight:400; }
    .asset-close { display:inline-flex; width:2.25rem; height:2.25rem; align-items:center; justify-content:center; border:0; border-radius:.375rem; background:transparent; color:var(--risu-theme-textcolor2); }
    .asset-close:hover { color:var(--risu-theme-textcolor); background:var(--risu-theme-selected); }
    .asset-close:focus-visible { outline:2px solid var(--risu-theme-borderc); outline-offset:2px; }
    .asset-tabs { padding:0 1rem; }
    .asset-tabs :global(.g-tabs) { margin-top:.75rem; }
    .asset-body { min-height:0; flex:1; overflow:auto; }
    .asset-body :global(.g-toolbar) { position:sticky; top:0; z-index:5; background:var(--risu-theme-bgcolor); }
    .asset-empty { text-align:center; padding:3rem 1rem; color:var(--risu-theme-textcolor2); }
    .asset-info { display:grid; grid-template-columns:auto minmax(0,1fr); gap:.375rem 1rem; font-size:13px; margin:0; }
    .asset-info dt { color:var(--risu-theme-textcolor2); }
    .asset-info dd { margin:0; font-variant-numeric:tabular-nums; overflow-wrap:anywhere; display:flex; align-items:center; gap:.5rem; flex-wrap:wrap; }
    .asset-exclude { display:flex; align-items:center; gap:.5rem; margin-top:1rem; font-size:.85rem; }
    .rename-overlay { position:fixed; inset:0; z-index:170; background:rgba(0,0,0,.5); display:flex; align-items:center; justify-content:center; padding:1rem; color:var(--risu-theme-textcolor); }
    .rename-dialog { width:100%; max-width:32rem; max-height:100%; overflow-y:auto; display:flex; flex-direction:column; gap:1rem; padding:1rem; border:0; border-radius:.375rem; background:var(--risu-theme-darkbg); }
    .rename-dialog h3 { font-size:1.25rem; font-weight:700; line-height:1.4; white-space:pre-wrap; margin:0; }
    .rename-field { display:grid; gap:.35rem; font-size:.875rem; }
    .rename-dialog input { width:100%; min-width:0; padding:.5rem 1rem; background:transparent; color:var(--risu-theme-textcolor); border:1px solid var(--risu-theme-darkborderc); border-radius:.375rem; font:inherit; }
    .rename-dialog input[aria-invalid='true'] { border-color:color-mix(in srgb,var(--risu-theme-danger-400) 70%,transparent); }
    .rename-dialog p { font-size:13px; line-height:1.45; white-space:pre-wrap; overflow-wrap:anywhere; color:var(--risu-theme-textcolor2); margin:0; }
    .rename-error { font-size:13px; color:var(--risu-theme-danger-400); }
    .rename-error:empty { display:none; }
    .rename-buttons { display:grid; grid-template-columns:1fr 1fr; gap:.5rem; }
    @media(max-width:640px) { .asset-overlay { padding:0; } .asset-panel { width:100%; height:100dvh; border:0; border-radius:0; } .asset-header { padding:.75rem 1rem 0; } }
</style>
