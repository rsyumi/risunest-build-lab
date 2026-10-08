import { isCompositionKey } from '../hotkeyModifier'
import type { MessageSendKey } from '../storage/deviceSettings'

export function shouldSendMessage(event: KeyboardEvent, mode: MessageSendKey): boolean {
    if (isCompositionKey(event) || event.key !== 'Enter' || event.altKey) return false
    if (mode === 'enter') return !event.shiftKey && !event.ctrlKey && !event.metaKey
    if (mode === 'ctrl-shift-enter') {
        return event.shiftKey
            ? !event.ctrlKey && !event.metaKey
            : event.ctrlKey !== event.metaKey
    }
    return false
}
