import { expect, it } from 'vitest'
import { shouldSendMessage } from './messageSendKey'

const cases: Array<{ name: string; keys: KeyboardEventInit; sends: string[] }> = [
    { name: 'Enter', keys: {}, sends: ['enter'] },
    { name: 'Shift+Enter', keys: { shiftKey: true }, sends: ['ctrl-shift-enter'] },
    { name: 'Ctrl+Enter', keys: { ctrlKey: true }, sends: ['ctrl-shift-enter'] },
    { name: 'Command+Enter', keys: { metaKey: true }, sends: ['ctrl-shift-enter'] },
    { name: 'Alt+Enter', keys: { altKey: true }, sends: [] },
    { name: 'Ctrl+Shift+Enter', keys: { ctrlKey: true, shiftKey: true }, sends: [] },
    { name: 'Command+Shift+Enter', keys: { metaKey: true, shiftKey: true }, sends: [] },
    { name: 'Ctrl+Command+Enter', keys: { ctrlKey: true, metaKey: true }, sends: [] },
    { name: 'Ctrl+Alt+Enter', keys: { ctrlKey: true, altKey: true }, sends: [] },
    { name: 'composing Enter', keys: { isComposing: true }, sends: [] },
    { name: 'composing Shift+Enter', keys: { isComposing: true, shiftKey: true }, sends: [] },
    { name: 'WebKit IME confirmation', keys: { keyCode: 229, ctrlKey: true }, sends: [] },
    { name: 'Ctrl+M', keys: { key: 'm', ctrlKey: true }, sends: [] },
]

it.each(cases)('$name sends only in the modes that select it', ({ keys, sends }) => {
    for (const mode of ['enter', 'ctrl-shift-enter', 'button'] as const) {
        const event = new KeyboardEvent('keydown', { key: 'Enter', ...keys })
        expect(shouldSendMessage(event, mode), mode).toBe(sends.includes(mode))
    }
})
