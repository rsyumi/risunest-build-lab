import '../../src/styles.css'
import { applyImageDimensionHints, bindImageReservations } from '../../src/ts/process/files/imageGeometryRender'
import { reconcileChatViewportChildren } from '../../src/ts/chatViewportDom'

export const chatRendering = { applyImageDimensionHints, bindImageReservations, reconcileChatViewportChildren }
declare global { interface Window { chatRendering: typeof chatRendering } }
window.chatRendering = chatRendering
