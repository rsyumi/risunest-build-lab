import { expect, it, vi } from 'vitest'
import { applyChatTemplate, chatTemplates } from './chatTemplate'

vi.mock('src/ts/storage/database.svelte', () => ({
    getDatabase: () => ({ instructChatTemplate: 'chatml', JinjaTemplate: '' }),
    getCurrentCharacter: () => ({ name: 'Synthetic Character' }),
}))
vi.mock('src/ts/util', () => ({ getUserName: () => 'Synthetic User' }))

const messages = [
    { role: 'system' as const, content: 'Synthetic system' },
    { role: 'user' as const, content: '  합성🐿️  ' },
    { role: 'assistant' as const, content: 'Synthetic reply' },
    { role: 'user' as const, content: 'Follow-up' },
]

it.each(Object.keys(chatTemplates))('preserves the exact %s prompt', (type) => {
    const original = type === 'gemma' ? messages.slice(1) : messages
    const input = structuredClone(original)
    expect(applyChatTemplate(input, { type })).toMatchSnapshot()
    expect(input).toEqual(original)
})

it('preserves Gemma role-alternation validation', () => {
    expect(() => applyChatTemplate(messages, { type: 'gemma' }))
        .toThrow('Conversation roles must alternate user/assistant/user/assistant/...')
})

it('preserves custom-template variables, whitespace and generation prompts', () => {
    expect(applyChatTemplate(messages, {
        type: 'jinja',
        custom: '{{ risu_char }}|{{ risu_user }}\n{% for message in messages %}[{{ loop.index }}:{{ message.role }}]{{ message.content | trim }}\n{% endfor %}{% if add_generation_prompt %}assistant:{% endif %}',
    })).toMatchSnapshot()
})

it('reports invalid custom templates', () => {
    expect(() => applyChatTemplate(messages, { type: 'jinja', custom: '{% if %}' })).toThrow()
})
