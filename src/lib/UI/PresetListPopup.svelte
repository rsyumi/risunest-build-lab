<script module lang="ts">
    export type PresetGuard = (work: () => Promise<void>) => Promise<void>
</script>

<script lang="ts" generics="T extends { name: string }">
    import type { Snippet } from 'svelte'
    import {
        CheckIcon,
        ChevronDownIcon,
        ChevronUpIcon,
        CopyIcon,
        EllipsisVerticalIcon,
        PencilIcon,
        TrashIcon,
        XIcon,
    } from '@lucide/svelte'
    import { language } from 'src/lang'
    import { alertInput, alertToast } from 'src/ts/alert'
    import { modalNavigation } from 'src/ts/ui/modalNavigation'

    interface Props {
        title: string
        close: () => void
        /** The stored list, created on first use. */
        list: () => T[]
        /** The stored list as last read, absent before it exists. */
        presets: T[] | undefined
        empty: string
        /** Shown when presets exist but the filter hides all of them. */
        emptyFiltered?: string
        filter?: (preset: T) => boolean
        movable?: boolean
        rowTitle: string
        subtitle?: (preset: T) => string
        selected?: (preset: T) => boolean
        apply: (preset: T) => Promise<void>
        copy: (preset: T, name: string) => T
        confirmDelete: (preset: T) => Promise<boolean>
        header?: Snippet<[PresetGuard]>
        leading?: Snippet<[PresetGuard]>
        rowActions?: Snippet<[T, number, PresetGuard, string]>
        footer?: Snippet<[PresetGuard, string]>
    }
    let {
        title,
        close,
        list,
        presets,
        empty,
        emptyFiltered,
        filter,
        movable = true,
        rowTitle,
        subtitle,
        selected,
        apply,
        copy,
        confirmDelete,
        header,
        leading,
        rowActions,
        footer,
    }: Props = $props()

    const titleId = $props.id()
    let openMenu = $state<number | null>(null)
    /** Nested alerts in progress. Escape and Back belong to them, not to this popup. */
    let busy = $state(0)
    let visible = $derived(
        (presets ?? [])
            .map((preset, index) => ({ preset, index }))
            .filter(({ preset }) => !filter || filter(preset)),
    )

    const guarded: PresetGuard = async (work) => {
        busy++
        try {
            await work()
        } finally {
            busy--
        }
    }
    const dismiss = () => {
        if (!busy) close()
    }

    const rename = (index: number) =>
        guarded(async () => {
            const preset = list()[index]
            if (!preset) return
            const name = (await alertInput(language.renamePreset, [], preset.name))?.trim()
            if (!name || name === preset.name) return
            if (!list().includes(preset)) return
            const previous = preset.name
            preset.name = name
            alertToast(language.presetRenamed(previous, name))
        })
    function duplicate(index: number) {
        const current = list()
        const preset = current[index]
        if (!preset) return
        const duplicated = copy(preset, `${preset.name} (Copy)`)
        current.splice(index + 1, 0, duplicated)
        openMenu = null
        alertToast(language.presetDuplicated(duplicated.name))
    }
    const remove = (index: number) =>
        guarded(async () => {
            const preset = list()[index]
            if (!preset || !(await confirmDelete(preset))) return
            const current = list()
            const currentIndex = current.indexOf(preset)
            if (currentIndex < 0) return
            current.splice(currentIndex, 1)
            openMenu = null
            alertToast(language.presetDeleted(preset.name))
        })
    function move(index: number, delta: -1 | 1) {
        const current = list()
        const target = index + delta
        if (target < 0 || target >= current.length) return
        ;[current[index], current[target]] = [current[target], current[index]]
        openMenu = target
    }

    const action =
        'inline-flex items-center justify-center gap-1.5 min-h-10 px-3 rounded-md border border-darkborderc bg-darkbutton text-sm text-textcolor transition-colors hover:bg-selected disabled:opacity-40 disabled:pointer-events-none'
    const small =
        'inline-flex items-center gap-1 h-8 px-2 rounded-md border border-darkborderc bg-darkbutton text-xs text-textcolor transition-colors hover:bg-selected disabled:opacity-40 disabled:pointer-events-none'
</script>

<div class="fixed inset-0 z-modal bg-black/50 flex justify-center items-center p-4" use:modalNavigation={{ close: dismiss }}>
    <div
        class="relative z-10 bg-darkbg text-textcolor rounded-md flex flex-col w-96 max-w-full max-h-full"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
    >
        <div class="flex items-center gap-2 p-4 pb-2">
            <h2 id={titleId} class="font-bold grow m-0">{title}</h2>
            <button class="text-textcolor2 hover:text-green-500 transition-colors" title={language.cancel} onclick={close}>
                <XIcon size={24} />
            </button>
        </div>
        <div class="flex flex-col gap-3 px-4 pb-4 overflow-y-auto">
            {@render header?.(guarded)}
            {#if leading}
                <div class="flex flex-col gap-1">
                    {@render leading(guarded)}
                </div>
            {/if}
            {#if !presets?.length}
                <p class="text-textcolor2 text-sm m-0">{empty}</p>
            {:else if visible.length === 0}
                <p class="text-textcolor2 text-sm m-0">{emptyFiltered ?? empty}</p>
            {:else}
                <div class="flex flex-col gap-1" role="list">
                    {#each visible as { preset, index } (index)}
                        <div
                            class="border rounded-md transition-colors {openMenu === index
                                ? 'border-selected'
                                : 'border-darkborderc'}"
                            role="listitem"
                        >
                            <div class="flex items-stretch">
                                <button
                                    class="flex-1 min-w-0 px-3 py-2 text-left rounded-l-md transition-colors hover:bg-selected flex items-center gap-2"
                                    title={rowTitle}
                                    aria-current={selected?.(preset) ? 'true' : undefined}
                                    onclick={() => guarded(() => apply(preset))}
                                >
                                    {#if selected}
                                        <span class="shrink-0 w-4 text-textcolor">
                                            {#if selected(preset)}<CheckIcon size={16} />{/if}
                                        </span>
                                    {/if}
                                    <span class="min-w-0 flex-1">
                                        {#if subtitle}
                                            <span class="block text-xs text-textcolor2 truncate">{subtitle(preset)}</span>
                                        {/if}
                                        <span class="block truncate {selected?.(preset) ? 'font-medium' : ''}">{preset.name}</span>
                                    </span>
                                </button>
                                <button
                                    class="shrink-0 w-9 flex items-center justify-center rounded-r-md transition-colors hover:bg-selected"
                                    title={language.presetActions}
                                    aria-expanded={openMenu === index}
                                    onclick={() => {
                                        openMenu = openMenu === index ? null : index
                                    }}><EllipsisVerticalIcon size={16} /></button
                                >
                            </div>
                            {#if openMenu === index}
                                <div class="flex flex-wrap gap-1 p-2 border-t border-darkborderc">
                                    {#if movable}
                                        <button
                                            class={small}
                                            title={language.movePresetUp}
                                            disabled={index === 0}
                                            onclick={() => move(index, -1)}><ChevronUpIcon size={14} /></button
                                        >
                                        <button
                                            class={small}
                                            title={language.movePresetDown}
                                            disabled={index === presets.length - 1}
                                            onclick={() => move(index, 1)}><ChevronDownIcon size={14} /></button
                                        >
                                    {/if}
                                    {@render rowActions?.(preset, index, guarded, small)}
                                    <button class={small} onclick={() => rename(index)}
                                        ><PencilIcon size={14} />{language.renamePreset}</button
                                    >
                                    <button class={small} onclick={() => duplicate(index)}
                                        ><CopyIcon size={14} />{language.duplicatePreset}</button
                                    >
                                    <button
                                        class="{small} text-draculared border-draculared/40 hover:bg-draculared/15"
                                        onclick={() => remove(index)}
                                        ><TrashIcon size={14} />{language.deletePreset}</button
                                    >
                                </div>
                            {/if}
                        </div>
                    {/each}
                </div>
            {/if}
            {@render footer?.(guarded, action)}
        </div>
    </div>
    <button class="absolute inset-0 cursor-default" aria-label={language.cancel} onclick={dismiss}></button>
</div>
