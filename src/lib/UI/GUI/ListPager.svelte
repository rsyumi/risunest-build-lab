<script lang="ts">
    import { ChevronLeftIcon, ChevronRightIcon } from '@lucide/svelte'
    import { language } from 'src/lang'
    let {
        page = $bindable(0),
        total,
        pageSize = 60,
        disabled = false,
    }: {
        page?: number
        total: number
        pageSize?: number
        disabled?: boolean
    } = $props()
    const pages = $derived(Math.max(1, Math.ceil(total / pageSize)))
    $effect(() => {
        if (page >= pages) page = pages - 1
    })
</script>

{#if pages > 1}
    <nav
        aria-label={language.risuNest.pager.pages}
        class="flex w-full items-center justify-center gap-3 py-2 text-sm text-textcolor2"
    >
        <button
            aria-label={language.risuNest.pager.previous}
            class="rounded-sm border border-darkborderc p-1 transition-colors hover:text-textcolor disabled:opacity-30"
            disabled={disabled || page === 0}
            onclick={() => page--}><ChevronLeftIcon size={18} /></button
        >
        <span class="tabular-nums">{page + 1} / {pages}</span>
        <button
            aria-label={language.risuNest.pager.next}
            class="rounded-sm border border-darkborderc p-1 transition-colors hover:text-textcolor disabled:opacity-30"
            disabled={disabled || page + 1 >= pages}
            onclick={() => page++}><ChevronRightIcon size={18} /></button
        >
    </nav>
{/if}
