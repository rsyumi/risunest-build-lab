<script lang="ts">
    import { language } from 'src/lang'
    import SettingButton from 'src/lib/Setting/RisuNest/SettingButton.svelte'

    interface Conflict {
        id: string
        kind: 'history-conflict' | 'history-budget' | 'archived-owner'
        canSaveSeparateCopy: boolean
        operationError?: unknown
    }
    type Action = 'save-separate-copy' | 'discard-covered-edits'
    type Outcome = { kind: 'resolved' | 'conflict-changed' | 'committed-refresh-required' }
    interface Props {
        conflict: Conflict | null
        resolve: (request: { id: string; action: Action }) => Promise<Outcome>
    }
    let { conflict, resolve }: Props = $props()
    let panel = $state<HTMLDivElement | undefined>()
    let busy = $state(false)
    let dialogState = $state<{ id: string; confirming?: boolean; failed?: boolean; settled?: boolean }>({ id: '' })
    const copy = $derived(language.risuNest.receiveConflict)
    const current = $derived(dialogState.id === conflict?.id ? dialogState : undefined)
    const disabled = $derived(busy || current?.settled === true)

    $effect(() => {
        if (conflict?.id) panel?.focus()
    })

    function confirmDiscard(): void {
        if (!conflict || disabled) return
        dialogState = { id: conflict.id, confirming: true }
    }

    async function act(action: Action): Promise<void> {
        if (!conflict || disabled) return
        if (action === 'save-separate-copy' && !conflict.canSaveSeparateCopy) return
        if (action === 'discard-covered-edits' && !current?.confirming) return
        const id = conflict.id
        busy = true
        dialogState = { id, confirming: action === 'discard-covered-edits' }
        try {
            const outcome = await resolve({ id, action })
            if (conflict?.id === id) {
                // The owner replaces the conflict or opens committed-refresh recovery.
                dialogState = { id, settled: outcome.kind !== 'conflict-changed' }
            }
        } catch {
            if (conflict?.id === id) dialogState = { id, failed: true }
        } finally {
            busy = false
        }
    }
</script>

{#if conflict}
    <div class="fixed inset-0 z-work-dialog-recovery flex items-center justify-center bg-black/60 p-4">
        <div bind:this={panel} tabindex="-1" role="dialog" aria-modal="true" aria-label={copy.title}
            class="flex max-h-[90dvh] w-full max-w-lg flex-col gap-4 overflow-y-auto rounded-lg border border-darkborderc bg-darkbg p-5 text-textcolor outline-hidden">
            <h2 class="text-lg font-bold">{copy.title}</h2>
            <p class="text-sm text-textcolor2">{conflict.kind === 'archived-owner' ? copy.archived : copy.unsaved}</p>
            {#if current?.failed || conflict.operationError != null}
                <p role="alert" class="text-sm text-danger-400">{copy.failed}</p>
            {/if}
            {#if current?.confirming}
                <p class="text-sm">{copy.discardConfirm}</p>
                <div class="flex flex-wrap justify-end gap-2">
                    <SettingButton variant="secondary" disabled={disabled} onclick={() => { dialogState = { id: conflict!.id } }}>{language.cancel}</SettingButton>
                    <SettingButton variant="danger" busy={busy} disabled={disabled} onclick={() => act('discard-covered-edits')}>{copy.discard}</SettingButton>
                </div>
            {:else}
                {#if conflict.canSaveSeparateCopy}
                    <p class="text-sm text-textcolor2">{copy.copyHelp}</p>
                {/if}
                <div class="flex flex-wrap justify-end gap-2">
                    <SettingButton variant="danger" disabled={disabled} onclick={confirmDiscard}>{copy.discard}</SettingButton>
                    {#if conflict.canSaveSeparateCopy}
                        <SettingButton busy={busy} disabled={disabled} onclick={() => act('save-separate-copy')}>{copy.saveCopy}</SettingButton>
                    {/if}
                </div>
            {/if}
        </div>
    </div>
{/if}
