import { mount, unmount } from 'svelte'
import QrScanOverlay from 'src/lib/Others/QrScanOverlay.svelte'
import type { QrScanTone } from './qrScanner'

/** Set on `<html>` while the scanning overlay is the only painted content (see `src/styles.css`). */
export const QR_SCAN_ATTRIBUTE = 'data-risunest-qr-scan'

export function showQrScanOverlay(cancel: () => void, tone: QrScanTone): () => void {
    const overlay = mount(QrScanOverlay, { target: document.body, props: { oncancel: cancel, tone } })
    document.documentElement.setAttribute(QR_SCAN_ATTRIBUTE, '')
    let shown = true
    return () => {
        if (!shown) return
        shown = false
        document.documentElement.removeAttribute(QR_SCAN_ATTRIBUTE)
        void unmount(overlay)
    }
}
