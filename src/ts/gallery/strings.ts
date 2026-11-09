import { language } from 'src/lang'
export function galleryText() {
    const t = language.risuNest.gallery
    return {
        ...t,
        image: language.image, select: language.select,
        selectAll: language.risuNest.dataHealth.selectAll, clear: language.playground.inlayDeselectAll,
        download: language.download, optimize: language.risuNest.inlay.optimizeSelected,
        remove: language.remove, cancel: language.cancel, add: language.add,
        rename: language.renameTogglePreset, close: language.close,
        icons: language.charIcon, emotions: language.emotionImage, assets: language.additionalAssets,
        empty: language.risuNest.inlay.inventoryEmpty, loadFailed: language.risuNest.inlay.inventoryLoadFailed,
        refresh: language.risuNest.inlay.inventoryRefresh,
        attachmentTab: t.tab, attachments: t.title, scanUsage: t.scan, usageLocations: t.locations,
        findUsage: t.findLocations, scanDetail: t.scanProgress, recheck: t.rescan,
        character: language.character, characterSettings: t.characterLocation, pluginData: t.pluginLocation, messageNumber: t.messageIndex,
        usageEmpty: t.noLocations, deleteAttachment: t.deleteInlayTitle,
        deleteAttachmentOne: t.deleteInlay, deleteAttachmentMany: t.deleteInlays,
    }
}
export function galleryFormat(text: string, ...values: (string | number)[]): string {
    return text.replace(/\{(\d+)\}/g, (match, index) => values[Number(index)]?.toString() ?? match)
}
