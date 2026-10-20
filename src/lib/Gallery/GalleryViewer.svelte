<script lang="ts">
    import { onMount, untrack, type Snippet } from 'svelte'
    import { get } from 'svelte/store'
    import { XIcon, InfoIcon, MaximizeIcon, MinimizeIcon, ChevronLeftIcon, ChevronRightIcon } from '@lucide/svelte'
    import { modalNavigation } from 'src/ts/ui/modalNavigation'
    import { alertStore } from 'src/ts/stores.svelte'
    import { isCompositionKey } from 'src/ts/hotkeyModifier'
    import { galleryText } from 'src/ts/gallery/strings'
    import { galleryBytes, type GalleryItem } from 'src/ts/gallery/types'
    import { resolveGalleryMedia, releaseGalleryMedia, type GalleryMediaSource } from 'src/ts/gallery/galleryMedia'
    import { subscribeNativeMediaEndpointChanges } from 'src/ts/storage/platformBlobStore'
    import GalleryMedia from './GalleryMedia.svelte'
    import './gallery.css'
    let { item, index, total, previous, next, onclose, onnavigate, info, infoRows, infoExtra, actions }: {
        item:GalleryItem; index:number; total:number; previous?:GalleryItem; next?:GalleryItem
        onclose:()=>void; onnavigate:(index:number)=>void; info?:Snippet; infoRows?:Snippet; infoExtra?:Snippet; actions?:Snippet
    } = $props()
    const text = galleryText()
    let showInfo = $state(false)
    let actualSize = $state(false)
    let mediaInfo = $state<GalleryMediaSource | null>(null)
    let endpoint = $state(0)
    let stage:HTMLDivElement
    let pan: {x:number;y:number;left:number;top:number;pointer:number;touch:boolean} | null = null
    let lastTap = 0
    let lastTapX = 0, lastTapY = 0
    $effect(() => { item.id; untrack(() => { actualSize = false; mediaInfo = null }) })
    function navigate(delta:number) { const target=index+delta; if(target>=0 && target<total) onnavigate(target) }
    onMount(() => {
        showInfo = window.innerWidth >= 640
        const unsubscribe = subscribeNativeMediaEndpointChanges(() => { endpoint++ })
        return unsubscribe
    })
    function key(event:KeyboardEvent) {
        const alert = get(alertStore)
        if (event.defaultPrevented || isCompositionKey(event) || alert.dialogPending || !['none','toast'].includes(alert.type)) return
        const target = event.target as HTMLElement
        if (target?.closest('input,textarea,select,video,audio,[contenteditable]:not([contenteditable="false"])')) return
        if (target?.closest('[role="dialog"]') !== event.currentTarget) return
        if(event.key==='ArrowLeft') { event.preventDefault(); navigate(-1) }
        if(event.key==='ArrowRight') { event.preventDefault(); navigate(1) }
    }
    $effect(() => {
        endpoint
        const neighbours = [previous,next].filter((value):value is GalleryItem => !!value && value.type==='image')
        let active=true
        const loaded:{source:GalleryMediaSource;image:HTMLImageElement}[]=[]
        for(const neighbour of neighbours) void resolveGalleryMedia(neighbour).then(source => {
            if(!source) return
            if(!active) { releaseGalleryMedia(source); return }
            const image=new Image(); image.decoding='async'; image.src=source.url; loaded.push({source,image})
        }).catch(()=>{})
        return () => { active=false; for(const {source,image} of loaded) { image.removeAttribute('src'); releaseGalleryMedia(source) } }
    })
    function start(event:PointerEvent) {
        if((event.target as HTMLElement).closest('button,video,audio')) return
        pan={x:event.clientX,y:event.clientY,left:stage.scrollLeft,top:stage.scrollTop,pointer:event.pointerId,touch:event.pointerType==='touch'}
        if(actualSize) stage.setPointerCapture(event.pointerId)
    }
    function move(event:PointerEvent) {
        if(!pan || !actualSize) return
        stage.scrollLeft=pan.left+pan.x-event.clientX
        stage.scrollTop=pan.top+pan.y-event.clientY
    }
    function end(event:PointerEvent) {
        if(!pan) return
        const current=pan; pan=null
        if(stage.hasPointerCapture(current.pointer)) stage.releasePointerCapture(current.pointer)
        const dx=event.clientX-current.x, dy=event.clientY-current.y
        if(Math.hypot(dx,dy)<12 && current.touch && item.type==='image') {
            const now=Date.now()
            if(now-lastTap<300 && Math.hypot(event.clientX-lastTapX,event.clientY-lastTapY)<30) { actualSize=!actualSize; lastTap=0 }
            else { lastTap=now; lastTapX=event.clientX; lastTapY=event.clientY }
        } else if(!actualSize && current.touch && Math.abs(dx)>50 && Math.abs(dx)>Math.abs(dy)*1.5) navigate(dx<0?1:-1)
    }
</script>
<div class="viewer" role="dialog" aria-modal="true" aria-label={item.name || item.key} tabindex="-1" onkeydown={key} use:modalNavigation={{close:onclose}}>
    <div class="main">
        <div class="top">
            <button type="button" class="vbutton icon" onclick={onclose} aria-label={text.close}><XIcon size={20} /></button>
            <div class="title"><div class="name">{item.name || item.key}</div><div class="count">{index+1} / {total}</div></div>
            {#if item.type==='image'}<button type="button" class="vbutton" class:active={actualSize} onclick={()=>{actualSize=!actualSize}}>{#if actualSize}<MinimizeIcon size={16}/>{:else}<MaximizeIcon size={16}/>{/if}{actualSize?text.fit:text.actualSize}</button>{/if}
            {#if actions}<div class="desktop-actions">{@render actions()}</div>{/if}
            <button type="button" class="vbutton" class:active={showInfo} aria-expanded={showInfo} onclick={()=>{showInfo=!showInfo}}><InfoIcon size={16}/>{text.info}</button>
        </div>
        <div class="stage-container">
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div bind:this={stage} class="stage" class:actual={actualSize} onpointerdown={start} onpointermove={move} onpointerup={end} onpointercancel={()=>{pan=null}} ondblclick={()=>{if(item.type==='image')actualSize=!actualSize}}>
                {#key item.id + ':' + item.key + ':' + item.ext}
                    <GalleryMedia {item} {actualSize} onmetadata={source=>{mediaInfo=source}} />
                {/key}
            </div>
            <button type="button" class="navigation previous" class:actual-nav={actualSize} aria-label={text.previous} disabled={index<=0} onclick={()=>navigate(-1)}><ChevronLeftIcon size={25}/></button>
            <button type="button" class="navigation next" class:actual-nav={actualSize} aria-label={text.next} disabled={index>=total-1} onclick={()=>navigate(1)}><ChevronRightIcon size={25}/></button>
        </div>
    </div>
    {#if showInfo}
        <aside class="information" aria-label={text.info}>
            <div class="handle"></div>
            {#if info}{@render info()}{:else}
                <dl>
                    <dt>{text.name}</dt><dd>{item.name || item.key}</dd>
                    <dt>{text.extension}</dt><dd>{item.ext.toUpperCase()}</dd>
                    {#if (mediaInfo?.width ?? item.width) && (mediaInfo?.height ?? item.height)}<dt>{text.resolution}</dt><dd>{mediaInfo?.width ?? item.width} × {mediaInfo?.height ?? item.height}</dd>{/if}
                    <dt>{text.size}</dt><dd>{galleryBytes(mediaInfo?.size ?? item.size)}</dd>
                    {#if item.sortTime && item.sortTime>0}<dt>{text.date}</dt><dd>{new Date(item.sortTime).toLocaleString()}</dd>{/if}
                    {#if infoRows}{@render infoRows()}{/if}
                </dl>
            {/if}
            {#if infoExtra}{@render infoExtra()}{/if}
            {#if actions}<div class="mobile-actions">{@render actions()}</div>{/if}
        </aside>
    {/if}
</div>
<style>
    .viewer { position:fixed; inset:0; z-index:150; display:flex; background:var(--risu-theme-bgcolor); color:var(--risu-theme-textcolor); padding-top:env(safe-area-inset-top,0px); padding-left:env(safe-area-inset-left,0px); padding-right:env(safe-area-inset-right,0px); }
    .main { position:relative; flex:1; min-width:0; min-height:0; display:flex; flex-direction:column; }
    .top { display:flex; flex-wrap:wrap; align-items:center; gap:.5rem; padding:.625rem .75rem; }
    .title { display:grid; min-width:3rem; flex:1; line-height:1.3; }
    .name { font-size:14px; font-weight:600; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
    .count { font-size:12px; color:var(--risu-theme-textcolor2); font-variant-numeric:tabular-nums; }
    .vbutton { display:inline-flex; align-items:center; justify-content:center; gap:.375rem; height:2.25rem; padding:0 .625rem; border-radius:.375rem; border:1px solid color-mix(in srgb,currentColor 14%,transparent); background:color-mix(in srgb,currentColor 6%,transparent); color:inherit; font-size:13px; white-space:nowrap; cursor:pointer; }
    .vbutton.icon { width:2.25rem; padding:0; }
    .vbutton.active,.vbutton:hover { background:color-mix(in srgb,currentColor 16%,transparent); }
    .vbutton:focus-visible,.navigation:focus-visible { outline:2px solid var(--risu-theme-borderc); outline-offset:2px; }
    .stage-container { position:relative; flex:1; min-height:0; }
    .stage { position:absolute; inset:0; display:flex; align-items:center; justify-content:center; padding:.5rem 4rem 1.5rem; overflow:hidden; touch-action:pan-y; }
    .stage.actual { display:block; overflow:auto; padding:0; touch-action:none; cursor:grab; }
    .stage.actual:active { cursor:grabbing; }
    .navigation { position:absolute; z-index:1; top:50%; transform:translateY(-50%); display:flex; width:2.75rem; height:2.75rem; align-items:center; justify-content:center; border-radius:999px; border:1px solid var(--risu-theme-darkborderc); background:color-mix(in srgb,var(--risu-theme-darkbg) 90%,transparent); color:var(--risu-theme-textcolor); cursor:pointer; }
    .navigation:disabled { visibility:hidden; }
    .previous { left:.75rem; }.next { right:.75rem; }
    .information { flex:none; width:20rem; border-left:1px solid var(--risu-theme-darkborderc); background:var(--risu-theme-darkbg); overflow-y:auto; padding:1rem; }
    dl { display:grid; grid-template-columns:auto minmax(0,1fr); gap:.375rem 1rem; font-size:13px; margin:0; }
    dt { color:var(--risu-theme-textcolor2); }dd { margin:0; overflow-wrap:anywhere; font-variant-numeric:tabular-nums; }
    .desktop-actions { display:flex; flex-wrap:wrap; gap:.5rem; }
    .desktop-actions :global(.g-button) { height:2.25rem; padding:0 .625rem; font-size:13px; }
    .mobile-actions,.handle { display:none; }
    @media(max-width:639px) {
        .viewer { flex-direction:column; }
        .stage { padding:.25rem .5rem .75rem; }
        .information { width:100%; max-height:58%; border-left:0; border-top:1px solid var(--risu-theme-darkborderc); border-radius:1rem 1rem 0 0; padding:.5rem 1rem max(1.5rem,env(safe-area-inset-bottom)); }
        .handle { display:block; width:2.5rem; height:.25rem; border-radius:999px; background:var(--risu-theme-darkborderc); margin:0 auto .75rem; }
        .desktop-actions { display:none; }
        .mobile-actions { display:flex; flex-wrap:wrap; gap:.5rem; margin-top:1.25rem; }
    }
</style>
