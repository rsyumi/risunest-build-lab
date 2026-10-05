import type {
    ActiveConversationBackwardScan,
    ActiveConversationWindow,
    MessageLocator,
} from '../storage/activeConversationSession'
import type { ConversationHistoryOperation } from '../storage/conversationHistoryOperation'
import type { Message } from '../storage/database.svelte'

/**
 * Presents a history operation over a window's messages at their absolute
 * positions. A read that starts before the window is answered from the window
 * start: an overlapping range returns its loaded part, and a range wholly
 * before the window returns the window's first messages, so callers that page
 * by `endIndex` move into the window.
 */
export class WindowedConversationHistoryOperation implements ConversationHistoryOperation {
    readonly totalMessages: number

    constructor(
        private readonly inner: ConversationHistoryOperation,
        readonly windowStart: number,
    ) {
        this.totalMessages = windowStart + inner.totalMessages
    }

    get source() { return this.inner.source }
    get characterId() { return this.inner.characterId }
    get conversationId() { return this.inner.conversationId }
    get storeRevision() { return this.inner.storeRevision }
    get sessionVersion() { return this.inner.sessionVersion }

    readLatest(limit: number): ActiveConversationWindow {
        const count = Math.min(limit, this.inner.totalMessages)
        if (count === 0 && limit > 0) return this.emptyWindow(this.totalMessages)
        return this.shiftWindow(this.inner.readLatest(count))
    }

    readRange(startIndex: number, limit: number): ActiveConversationWindow {
        const localStart = startIndex - this.windowStart
        if (localStart >= 0) return this.shiftWindow(this.inner.readRange(localStart, limit))
        // A range that starts before the window continues at the window start.
        const count = Math.min(Math.max(limit + localStart, 0) || limit, this.inner.totalMessages)
        if (count === 0) return this.emptyWindow(this.windowStart)
        return this.shiftWindow(this.inner.readRange(0, count))
    }

    scanBackward(
        startIndexExclusive = this.totalMessages,
        limit?: number,
    ): ActiveConversationBackwardScan {
        const localStart = Math.max(startIndexExclusive - this.windowStart, 0)
        const scan = limit === undefined
            ? this.inner.scanBackward(localStart)
            : this.inner.scanBackward(localStart, limit)
        return {
            ...scan,
            entries: scan.entries.map((entry) => ({
                ...entry,
                absoluteIndex: entry.absoluteIndex + this.windowStart,
            })),
            startIndexExclusive: scan.startIndexExclusive + this.windowStart,
            totalMessages: this.totalMessages,
        }
    }

    resolveMessage(locator: MessageLocator): Message {
        return this.inner.resolveMessage(locator)
    }

    ensureMessageId(locator: MessageLocator, createId: () => string): Message {
        return this.inner.ensureMessageId(locator, createId)
    }

    assertCurrent(): void {
        this.inner.assertCurrent()
    }

    dispose(): void {
        this.inner.dispose()
    }

    private emptyWindow(index: number): ActiveConversationWindow {
        return {
            characterId: this.characterId,
            conversationId: this.conversationId,
            messages: [],
            locators: [],
            startIndex: index,
            endIndex: index,
            totalMessages: this.totalMessages,
            storeRevision: this.storeRevision,
            sessionVersion: this.sessionVersion,
        }
    }

    private shiftWindow(window: ActiveConversationWindow): ActiveConversationWindow {
        return {
            ...window,
            startIndex: window.startIndex + this.windowStart,
            endIndex: window.endIndex + this.windowStart,
            totalMessages: this.totalMessages,
        }
    }
}
