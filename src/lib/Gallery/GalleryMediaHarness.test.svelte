<script lang="ts">
    import { untrack } from 'svelte'
    import GalleryMedia from './GalleryMedia.svelte'
    import type { GalleryItem } from 'src/ts/gallery/types'
    import type { GalleryMediaSource } from 'src/ts/gallery/galleryMedia'
    let { initial, onmetadata }: { initial:GalleryItem; onmetadata?:(source:GalleryMediaSource)=>void } = $props()
    let item = $state(untrack(() => initial))
    let preview = $state(false)
    let callback = $state(untrack(() => onmetadata))
    export function update(next:GalleryItem) { item = next }
    export function setPreview(value:boolean) { preview = value }
    export function setCallback(value:(source:GalleryMediaSource)=>void) { callback = value }
</script>
<GalleryMedia {item} {preview} onmetadata={callback} />
