<script lang="ts">
    import type { Snippet } from 'svelte'
    import { galleryText, galleryFormat } from 'src/ts/gallery/strings'
    import './gallery.css'
    let { selectedCount, total, onselectall, onclear, children }: {
        selectedCount:number; total:number; onselectall:()=>void; onclear:()=>void; children?:Snippet
    } = $props()
    const text = galleryText()
    const allSelected = $derived(selectedCount > 0 && selectedCount === total)
    const someSelected = $derived(selectedCount > 0 && selectedCount < total)
    function change(event: Event & { currentTarget: HTMLInputElement }) {
        // The selection drives the box; an unchanged selection would leave the browser's toggle in place.
        event.currentTarget.checked = allSelected
        event.currentTarget.indeterminate = someSelected
        if (allSelected) onclear()
        else onselectall()
    }
</script>
<div class="g-toolbar" data-gallery-selection data-gallery-toolbar>
    <label class="select-all"><input type="checkbox" checked={allSelected} indeterminate={someSelected} onchange={change} aria-label={text.selectAll} /><span>{text.selectAll}</span></label>
    <span class="count">{galleryFormat(text.selected, selectedCount)}</span>
    <div class="g-grow"></div>
    {#if children}{@render children()}{/if}
    <button type="button" class="g-button small quiet" onclick={onclear}>{text.clear}</button>
</div>
<style>
    .select-all { display:flex; align-items:center; gap:.5rem; font-size:.875rem; cursor:pointer; }
    input { width:1.5rem; height:1.5rem; accent-color:var(--risu-theme-borderc); }
    .count { font-size:.875rem; font-variant-numeric:tabular-nums; }
</style>
