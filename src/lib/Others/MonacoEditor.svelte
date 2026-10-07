<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import * as monaco from 'monaco-editor';
    import { registerCBSMonaco } from 'src/ts/gui/codearea/cbsMonaco';
    import EditorWorker from 'monaco-editor/editor/editor.worker?worker';

    // Set up workers once globally
    if (!('MonacoEnvironment' in self)) {
        (self as any).MonacoEnvironment = {
            getWorker() {
                return new EditorWorker();
            }
        };
    }


    registerCBSMonaco()

    interface Props {
        value: string;
        language?: string;
        theme?: string;
        readonly?: boolean;
        onchange?: (value: string) => void;
        options?: monaco.editor.IStandaloneEditorConstructionOptions;
        onready?: (editor: monaco.editor.IStandaloneCodeEditor) => void;
    }

    let {
        value = $bindable(''),
        language = 'markdown',
        theme = 'vs-dark',
        readonly = false,
        onchange,
        options = {},
        onready,
    }: Props = $props();

    let container: HTMLDivElement;
    let editor: monaco.editor.IStandaloneCodeEditor;
    let compositionOffscreen = $state(false);

    onMount(() => {
        editor = monaco.editor.create(container, {
            value,
            language,
            theme,
            readOnly: readonly,
            editContext: false,
            automaticLayout: true,
            minimap: { enabled: false },
            wordWrap: 'on',
            lineNumbers: 'on',
            scrollBeyondLastLine: false,
            fontSize: 14,
            fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
            padding: { top: 8, bottom: 8 },
            renderLineHighlight: 'gutter',
            overviewRulerBorder: false,
            scrollbar: {
                verticalScrollbarSize: 6,
                horizontalScrollbarSize: 6,
            },
            ...options,
        });

        editor.onDidChangeModelContent(() => {
            const newValue = editor.getValue();
            value = newValue;
            onchange?.(newValue);
        });
        // Monaco stops moving its composition textarea once the composed line leaves the viewport.
        const trackCompositionLine = () => {
            const position = editor.getPosition();
            const visible = position && editor.getScrolledVisiblePosition(position);
            compositionOffscreen = !visible || visible.top + visible.height <= 0 || visible.top >= editor.getLayoutInfo().height;
        };
        editor.onDidScrollChange(trackCompositionLine);
        editor.onDidChangeCursorPosition(trackCompositionLine);
        onready?.(editor);

        return () => {
            editor?.dispose();
        };
    });

    onDestroy(() => {
        editor?.dispose();
    });

    // Sync external value changes into editor without triggering onDidChangeModelContent loop
    $effect(() => {
        if (editor && editor.getValue() !== value) {
            const model = editor.getModel();
            if (model) {
                model.pushEditOperations(
                    [],
                    [{ range: model.getFullModelRange(), text: value }],
                    () => null
                );
            }
        }
    });
</script>

<div bind:this={container} class="w-full h-full" class:composition-offscreen={compositionOffscreen}></div>

<style>
    /* The app font reaches the rendered lines but not the composition textarea, whose font Monaco sets inline. */
    div :global(.monaco-editor .inputarea.ime-input) {
        font-family: var(--risu-font-family) !important;
    }
    .composition-offscreen :global(.monaco-editor .inputarea.ime-input) {
        opacity: 0;
    }
</style>
