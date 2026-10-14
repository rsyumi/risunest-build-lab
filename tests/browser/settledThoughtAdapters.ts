// The fixture has no character scripts, assets, translation, or persistent data.
// Parser, sanitizer, Markdown libraries, ChatBody and preview rendering stay real.
export const DBState = { db: {
    characters: [], autoTranslate: false, hideAllImages: true,
    unformatQuotes: true, newImageHandlingBeta: false,
} }
export const selIdState = { selId: -1 }
export const language = { cot: 'Synthetic thought', toolCalled: '{{tool}}' }
export const aiWatermarkingLawApplies = () => false
export class DeferredInlayMarkerRegistry { clear() {} }
export const resolveDeferredInlaySources = async () => () => {}
