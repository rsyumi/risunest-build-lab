export interface BoundedGenerationAdmission {
    selected: boolean
    hypaEnabled: boolean
    windowed: boolean
    observableTokenizer: boolean
    experimental: boolean
    editprocess: boolean
    editoutput: boolean
    chatOutput: boolean
    triggers: boolean
    historyRegex: boolean
    lorebooks: boolean
    additionalText: boolean
    inlayView: boolean
    dynamicHistory: boolean
    explicitHistory: boolean
}

export function boundedGenerationFallbackReason(input: BoundedGenerationAdmission): string | null {
    if (!input.selected) return 'selected-conversation-unavailable'
    if (!input.hypaEnabled) return 'standard-hypa-v3-disabled'
    if (!input.windowed) return 'selected-conversation-not-windowed'
    if (input.observableTokenizer) return 'custom-or-remote-tokenizer'
    if (input.experimental) return 'experimental-hypa-v3'
    if (input.editprocess) return 'plugin-editprocess'
    if (input.editoutput) return 'plugin-editoutput'
    if (input.chatOutput) return 'plugin-chat-output'
    if (input.triggers) return 'generation-trigger'
    if (input.historyRegex) return 'history-regex'
    if (input.lorebooks) return 'old-history-lorebook-consumer'
    if (input.additionalText) return 'additional-text-consumer'
    if (input.inlayView) return 'inlay-view-consumer'
    if (input.dynamicHistory) return 'dynamic-history-indirection'
    if (input.explicitHistory) return 'explicit-history-reference'
    return null
}
