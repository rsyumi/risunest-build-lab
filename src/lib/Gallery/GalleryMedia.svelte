<script lang="ts">
    import { onMount, untrack } from 'svelte'
    import { FileIcon, Music2Icon, PlayIcon, ImageOffIcon } from '@lucide/svelte'
    import type { GalleryItem } from 'src/ts/gallery/types'
    import { galleryText } from 'src/ts/gallery/strings'
    import { resolveGalleryMedia, releaseGalleryMedia, type GalleryMediaSource } from 'src/ts/gallery/galleryMedia'
    import { subscribeNativeMediaEndpointChanges } from 'src/ts/storage/platformBlobStore'
    import { observeNearViewport } from 'src/ts/ui/observeNearViewport'
    let { item, preview = false, actualSize = false, onmetadata }: {
        item:GalleryItem; preview?:boolean; actualSize?:boolean
        onmetadata?:(source:GalleryMediaSource)=>void
    } = $props()
    let root:HTMLDivElement
    let near = $state(false)
    let endpoint = $state(0)
    let source = $state<GalleryMediaSource | null>(null)
    let failed = $state(false)
    let playback = $state<HTMLMediaElement | undefined>()
    const mediaIdentity = $derived(JSON.stringify([item.kind, item.key, item.type, item.mime, item.ext,
        item.objectHash ?? item.contentRevision]))
    const text = galleryText()
    onMount(() => {
        let scrollRoot:HTMLElement | null = null
        if (preview) {
            scrollRoot = root.parentElement
            while (scrollRoot && !/(auto|scroll)/.test(getComputedStyle(scrollRoot).overflowY)) scrollRoot = scrollRoot.parentElement
        }
        const stop = preview ? observeNearViewport(root, () => { near = true }, scrollRoot) : () => {}
        if (!preview) near = true
        const unsubscribe = subscribeNativeMediaEndpointChanges(() => { endpoint++ })
        return () => { stop(); unsubscribe() }
    })
    $effect(() => {
        endpoint
        mediaIdentity
        const current = untrack(() => item)
        const usePreview = preview
        failed = false
        source = null
        if (!near || current.type === 'other' || (usePreview && current.type === 'audio')) return
        let active = true
        let resolved:GalleryMediaSource | null = null
        void resolveGalleryMedia(current, usePreview).then(value => {
            if (!active) { releaseGalleryMedia(value); return }
            resolved = value
            source = value
            failed = !value
        }).catch(() => { if (active) failed = true })
        return () => {
            active = false
            if (playback) { playback.pause(); playback.removeAttribute('src'); playback.load() }
            releaseGalleryMedia(resolved)
        }
    })
    $effect(() => {
        if (!source) return
        const metadata = { ...source, width:item.width ?? source.width, height:item.height ?? source.height,
            size:item.size || source.size }
        untrack(() => onmetadata?.(metadata))
    })
    function releaseImage(node:HTMLImageElement) {
        return { destroy() { node.removeAttribute('src') } }
    }
    function loaded(event:Event) {
        const img = event.currentTarget as HTMLImageElement
        if (!source || source.thumbnail || !img.naturalWidth || !img.naturalHeight) return
        void source.recordDimensions?.(img.naturalWidth, img.naturalHeight).catch(() => {})
        onmetadata?.({ ...source, width:img.naturalWidth, height:img.naturalHeight })
    }
</script>
<div bind:this={root} class="media" class:preview class:actual={actualSize}>
    {#if failed}
        <div class="unavailable"><ImageOffIcon size={24} aria-hidden="true" /><span>{text.unavailable}</span></div>
    {:else if item.type === 'other'}
        <div class="icon"><FileIcon size={36} aria-hidden="true" /><span>{item.ext.toUpperCase()}</span></div>
    {:else if preview && item.type === 'audio'}
        <div class="icon"><Music2Icon size={36} aria-hidden="true" /><span>{item.ext.toUpperCase()}</span></div>
    {:else if source}
        {#if item.type === 'image'}
            <img use:releaseImage src={source.url} alt={item.name} width={item.width ?? source.width} height={item.height ?? source.height} decoding="async" draggable="false" onload={loaded} onerror={() => { failed = true }} />
        {:else if item.type === 'video'}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video bind:this={playback} src={source.url} controls={!preview} muted={preview} playsinline preload={preview ? 'metadata' : 'auto'} onerror={() => { failed = true }}></video>
        {:else if item.type === 'audio'}
            <audio bind:this={playback} src={source.url} controls preload="metadata" onerror={() => { failed = true }}></audio>
        {/if}
    {:else}
        <div class="placeholder" style:aspect-ratio={item.width && item.height ? `${item.width}/${item.height}` : undefined}></div>
    {/if}
    {#if preview && item.type === 'video'}<span class="badge"><PlayIcon size={10} fill="currentColor" />{text.video}</span>{/if}
    {#if preview && item.type === 'audio'}<span class="badge"><Music2Icon size={10} />{text.audio}</span>{/if}
</div>
<style>
    .media { width:100%; height:100%; display:flex; align-items:center; justify-content:center; min-width:0; min-height:0; }
    img,video { max-width:100%; max-height:100%; width:auto; height:auto; object-fit:contain; border-radius:.1875rem; }
    .preview { padding:.375rem; }
    .preview img { box-shadow:0 1px 3px color-mix(in srgb,var(--risu-theme-darkborderc) 40%,transparent); }
    .preview video { pointer-events:none; }
    audio { max-width:100%; }
    .actual { display:block; width:max-content; height:max-content; min-width:100%; min-height:100%; }
    .actual img { display:block; max-width:none; max-height:none; border-radius:0; width:auto; height:auto; }
    .placeholder { background:color-mix(in srgb,var(--risu-theme-selected) 25%,transparent); max-width:100%; max-height:100%; width:70%; height:70%; border-radius:.1875rem; }
    .icon { display:flex; flex-direction:column; align-items:center; gap:.25rem; color:var(--risu-theme-textcolor2); font-size:12px; }
    .unavailable { display:flex; flex-direction:column; align-items:center; justify-content:center; gap:.5rem; color:var(--risu-theme-textcolor2); font-size:12px; text-align:center; padding:.5rem; }
    .badge { position:absolute; left:.375rem; bottom:.375rem; display:flex; align-items:center; gap:.25rem; border-radius:999px; background:var(--risu-theme-darkbg); color:var(--risu-theme-textcolor); padding:.125rem .4rem; font-size:11px; line-height:1; }
</style>
