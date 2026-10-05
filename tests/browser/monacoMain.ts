import { mount, unmount } from 'svelte'
import * as monaco from 'monaco-editor'
import TextEditorMonaco from '../../src/lib/Others/TextEditorMonaco.svelte'

let component: ReturnType<typeof mount> | undefined
let saved: string[] = []
let value = ''
const current = () => monaco.editor.getEditors().find(editor => editor.getDomNode()?.closest('#editor'))!

export const monacoDriver = {
    async open(language: 'markdown' | 'lua') {
        if (component) await unmount(component)
        saved = []
        value = language === 'lua' ? 'local greeting = "hello"\nprint(greeting)' : '# Synthetic title\nhello world'
        component = mount(TextEditorMonaco, {
            target: document.querySelector('#editor')!,
            props: { value, language, onchange: next => { value = next }, onsave: () => { saved.push(value) } },
        })
    },
    state: () => ({
        value: current()?.getValue(), saved,
        language: current()?.getModel()?.getLanguageId(),
        editContext: current()?.getOption(monaco.editor.EditorOption.editContext),
        languages: monaco.languages.getLanguages().map(language => language.id).sort(),
    }),
    async diff() {
        const original = monaco.editor.createModel('alpha\nbeta\ngamma', 'markdown')
        const modified = monaco.editor.createModel('alpha\nchanged\ngamma', 'markdown')
        const editor = monaco.editor.createDiffEditor(document.querySelector('#diff')!, { automaticLayout: true, editContext: false })
        try {
            await new Promise<void>(resolve => {
                const subscription = editor.onDidUpdateDiff(() => { subscription.dispose(); resolve() })
                editor.setModel({ original, modified })
            })
            return editor.getLineChanges()?.map(change => ({ original: change.originalStartLineNumber, modified: change.modifiedStartLineNumber }))
        } finally {
            editor.dispose()
            original.dispose()
            modified.dispose()
        }
    },
}

declare global { interface Window { monacoDriver: typeof monacoDriver } }
window.monacoDriver = monacoDriver
