<script lang="ts">
    import { KeyCode, KeyMod, editor as monacoEditor } from 'monaco-editor'
    import { ColorSchemeTypeStore } from 'src/ts/gui/colorscheme'
    import { language as uiLanguage } from 'src/lang'
    import MonacoEditor from './MonacoEditor.svelte'

    interface Props {
        value: string
        language: string
        onchange: (value: string) => void
        onsave: () => void
    }

    let { value = $bindable(), language, onchange, onsave }: Props = $props()
    let editor: monacoEditor.IStandaloneCodeEditor | undefined

    // The dialog draws the background, so the editor lets it show through.
    monacoEditor.defineTheme('risunest-dark', { base: 'vs-dark', inherit: true, rules: [], colors: { 'editor.background': '#00000000' } })
    monacoEditor.defineTheme('risunest-light', { base: 'vs', inherit: true, rules: [], colors: { 'editor.background': '#00000000' } })
    const theme = $derived($ColorSchemeTypeStore === 'light' ? 'risunest-light' : 'risunest-dark')
    $effect(() => monacoEditor.setTheme(theme))

    // Prose reads better without word completion, occurrence boxes and warnings on curly quotes.
    const proseOptions: monacoEditor.IStandaloneEditorConstructionOptions = {
        quickSuggestions: false,
        wordBasedSuggestions: 'off',
        occurrencesHighlight: 'off',
        unicodeHighlight: { ambiguousCharacters: false },
    }

    export function focus() {
        editor?.focus()
    }
</script>

<div class="text-editor-monaco">
    <MonacoEditor
        bind:value
        {language}
        {theme}
        {onchange}
        options={{ stickyScroll: { enabled: false }, ...(language === 'markdown' ? proseOptions : {}) }}
        onready={(created) => {
            editor = created
            // Monaco binds Ctrl+Enter to inserting a line; here it saves like the plain editor.
            const save = created.addAction({ id: 'risunest.save', label: uiLanguage.risuNest.textEditor.save, keybindings: [KeyMod.CtrlCmd | KeyCode.Enter], run: onsave })
            created.onDidDispose(() => save.dispose())
            created.focus()
        }}
    />
</div>

<style>
    .text-editor-monaco {
        display: contents;
    }
    /* Monaco hides the line under an IME composition with the editor background, which is transparent here. */
    .text-editor-monaco :global(.monaco-editor .inputarea.ime-input) {
        background-color: var(--risu-theme-darkbg);
    }
</style>
