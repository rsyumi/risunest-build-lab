<script lang="ts">
    import { onMount, tick, untrack } from 'svelte'
    import { CheckIcon } from '@lucide/svelte'
    import { galleryWindow } from 'src/ts/gallery/galleryWindow'
    import { galleryText } from 'src/ts/gallery/strings'
    import type { GalleryItem, GallerySize } from 'src/ts/gallery/types'
    import GalleryMedia from './GalleryMedia.svelte'
    let { total, items, size = 'normal', captions = false, selected, selecting = false, onwindow, onopen, ontoggle }: {
        total:number; items:Map<number,GalleryItem>; size?:GallerySize; captions?:boolean
        selected:Set<string>; selecting?:boolean
        onwindow:(offset:number,limit:number)=>void
        onopen:(item:GalleryItem,index:number)=>void
        ontoggle:(item:GalleryItem,index:number,range:boolean)=>void
    } = $props()
    let root:HTMLDivElement
    let scroller:HTMLElement | null = null
    let width = $state(600)
    let viewportHeight = $state(700)
    let scrollOffset = $state(0)
    let mobile = $state(false)
    let mounted = $state(false)
    let touchSelection = $state(false)
    let pendingFocus = $state<number | null>(null)
    let focusRequest = 0
    let suppressClick = false
    let press:ReturnType<typeof setTimeout> | undefined
    let pressX = 0, pressY = 0
    const text = galleryText()
    const layout = $derived(galleryWindow({ total,width,viewportHeight,scrollOffset,large:size === 'large',mobile,captions }))
    const indices = $derived(Array.from({ length:layout.limit },(_,i) => layout.offset + i))
    function viewportTop() {
        const top = Math.max(0,scroller?.getBoundingClientRect().top ?? 0)
        let inset = 0
        for (const toolbar of (scroller ?? document).querySelectorAll<HTMLElement>('[data-gallery-toolbar]')) {
            const style = getComputedStyle(toolbar)
            if (style.position !== 'sticky' || !toolbar.parentElement?.contains(root)) continue
            inset = Math.max(inset, (parseFloat(style.top) || 0) + toolbar.getBoundingClientRect().height)
        }
        return top + inset
    }
    function measure() {
        if (!root) return
        const bounds = root.getBoundingClientRect()
        const scrollBounds = scroller?.getBoundingClientRect()
        const top = viewportTop()
        const bottom = Math.min(window.innerHeight,scrollBounds?.bottom ?? window.innerHeight)
        width = bounds.width
        viewportHeight = Math.max(1,bottom-top)
        scrollOffset = Math.max(0,top-bounds.top-layout.padding)
        mobile = window.innerWidth < 640
    }
    onMount(() => {
        let parent = root.parentElement
        while (parent) {
            if (/(auto|scroll)/.test(getComputedStyle(parent).overflowY)) { scroller = parent; break }
            parent = parent.parentElement
        }
        let frame = 0
        const update = () => { if (!frame) frame = requestAnimationFrame(() => { frame = 0; measure() }) }
        const resize = new ResizeObserver(update)
        resize.observe(root)
        if (scroller) resize.observe(scroller)
        window.addEventListener('scroll',update,true)
        window.addEventListener('resize',update)
        measure(); mounted = true
        return () => { focusRequest++; pendingFocus = null; resize.disconnect(); cancelAnimationFrame(frame); clearTimeout(press); window.removeEventListener('scroll',update,true); window.removeEventListener('resize',update) }
    })
    $effect(() => {
        if (!mounted) return
        const {offset,limit,columns} = layout
        untrack(() => onwindow(offset,limit || columns * 8))
    })
    export async function scrollToIndex(index:number) {
        focusRequest++; pendingFocus = null
        await scrollIndex(index)
    }
    async function scrollIndex(index:number) {
        const row = Math.floor(Math.max(0,Math.min(total-1,index))/layout.columns)
        const rect = root.getBoundingClientRect()
        const top = viewportTop()
        const delta = rect.top - top + layout.padding + row * layout.rowHeight
        if (scroller) scroller.scrollTop += delta
        else window.scrollBy(0,delta)
        measure(); await tick()
    }
    export async function focusItem(index:number) {
        const request = ++focusRequest
        pendingFocus = null
        if (!total) return
        const target = Math.max(0,Math.min(total-1,index))
        if (!root.querySelector(`[data-gallery-index="${target}"]`)) await scrollIndex(target)
        if (request === focusRequest) pendingFocus = target
    }
    $effect(() => {
        if (pendingFocus !== null && pendingFocus >= total) pendingFocus = null
        if (pendingFocus === null || !items.has(pendingFocus) || !indices.includes(pendingFocus)) return
        const target = pendingFocus
        void tick().then(() => {
            if (pendingFocus !== target) return
            const button = root.querySelector<HTMLButtonElement>(`[data-gallery-index="${target}"]`)
            if (button) { button.focus({ preventScroll:true }); pendingFocus = null }
        })
    })
    $effect(() => { if (!selecting && selected.size === 0) touchSelection = false })
    function click(event:MouseEvent,item:GalleryItem,index:number) {
        focusRequest++; pendingFocus = null
        if (suppressClick) { suppressClick = false; return }
        if (selecting || (touchSelection && selected.size > 0) || event.ctrlKey || event.metaKey || event.shiftKey) ontoggle(item,index,event.shiftKey)
        else onopen(item,index)
    }
    function pointerDown(event:PointerEvent,item:GalleryItem,index:number) {
        if (event.pointerType === 'mouse') return
        suppressClick = false; pressX = event.clientX; pressY = event.clientY
        clearTimeout(press)
        press = setTimeout(() => { touchSelection = true; suppressClick = true; ontoggle(item,index,false) },500)
    }
    function pointerMove(event:PointerEvent) { if (Math.hypot(event.clientX-pressX,event.clientY-pressY)>10) clearTimeout(press) }
</script>
<div bind:this={root} class="gallery-grid" data-gallery-grid style:height={`${layout.height}px`}>
    <div class="window" style:grid-template-columns={`repeat(${layout.columns},minmax(0,1fr))`} style:gap={`${layout.gap}px`} style:top={`${layout.padding+layout.firstRow*layout.rowHeight}px`} style:left={`${layout.padding}px`} style:right={`${layout.padding}px`}>
        {#each indices as index (items.get(index)?.id ?? `pending-${index}`)}
            {@const item = items.get(index)}
            <div class="entry" style:height={`${layout.rowHeight-layout.gap}px`}>
                {#if item}
                    <div class="cell" class:selected={selected.has(item.id)}>
                        <button type="button" data-gallery-index={index} data-gallery-id={item.id} aria-label={item.name || item.key} class="open" onclick={event => click(event,item,index)} onpointerdown={event => pointerDown(event,item,index)} onpointermove={pointerMove} onpointerup={() => clearTimeout(press)} onpointercancel={() => clearTimeout(press)} oncontextmenu={event => { if (suppressClick) event.preventDefault() }}>
                            <GalleryMedia {item} preview />
                        </button>
                        <button type="button" class="checkbox" class:shown={selecting || selected.size > 0} class:checked={selected.has(item.id)} aria-label={`${text.select}: ${item.name || item.key}`} aria-pressed={selected.has(item.id)} onclick={event => { event.stopPropagation(); ontoggle(item,index,event.shiftKey) }}>{#if selected.has(item.id)}<CheckIcon size={14} strokeWidth={3} />{/if}</button>
                        {#if item.portrait}<span class="portrait">{text.currentIcon}</span>{/if}
                    </div>
                    {#if captions}<div class="caption" title={item.name}>{item.name}</div>{/if}
                {:else}<div class="cell placeholder" aria-hidden="true"></div>{/if}
            </div>
        {/each}
    </div>
</div>
<style>
    .gallery-grid { position:relative; width:100%; min-height:1px; overflow-anchor:none; }
    .window { position:absolute; display:grid; align-items:start; }
    .entry { display:flex; flex-direction:column; min-width:0; gap:4px; }
    .cell { position:relative; width:100%; aspect-ratio:1; flex:none; border-radius:.375rem; background:var(--risu-theme-bgcolor); border-width:1px; border-style:solid; border-color:color-mix(in srgb,var(--risu-theme-darkborderc) 55%,transparent); overflow:hidden; }
    .cell.selected { outline:2px solid var(--risu-theme-borderc); outline-offset:-2px; }
    .open { width:100%; height:100%; position:absolute; inset:0; cursor:pointer; -webkit-touch-callout:none; user-select:none; -webkit-user-select:none; }
    .open:focus-visible { outline:2px solid var(--risu-theme-borderc); outline-offset:-3px; }
    .checkbox { position:absolute; z-index:1; top:.375rem; left:.375rem; display:flex; width:1.75rem; height:1.75rem; align-items:center; justify-content:center; border:2px solid var(--risu-theme-darkborderc); border-radius:.375rem; background:color-mix(in srgb,var(--risu-theme-darkbutton) 88%,transparent); color:var(--risu-theme-textcolor); opacity:0; cursor:pointer; }
    .cell:hover .checkbox,.checkbox:focus-visible,.checkbox.shown { opacity:1; }
    .checkbox.checked { background:var(--risu-theme-selected); border-color:var(--risu-theme-borderc); }
    .caption { font-size:13px; line-height:18px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; padding:0 .125rem; }
    .portrait { position:absolute; right:.375rem; top:.375rem; padding:.0625rem .45rem; border-radius:999px; border:1px solid var(--risu-theme-darkborderc); background:var(--risu-theme-darkbg); font-size:11px; font-weight:600; color:var(--risu-theme-textcolor2); pointer-events:none; }
    @media (hover:none) { .checkbox { opacity:1; } }
</style>
