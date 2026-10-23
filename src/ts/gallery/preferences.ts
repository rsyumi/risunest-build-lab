import { writable } from 'svelte/store'
import type { GallerySize } from './types'
const key = 'risunest-gallery-size'
function read(): GallerySize {
    try { return localStorage.getItem(key) === 'large' ? 'large' : 'normal' } catch { return 'normal' }
}
export const gallerySize = writable<GallerySize>(read())
gallerySize.subscribe(size => { try { localStorage.setItem(key, size) } catch { /* Storage can be unavailable in private browsing. */ } })
