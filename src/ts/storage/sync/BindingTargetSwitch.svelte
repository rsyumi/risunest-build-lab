<script lang="ts">
    import { bindSyncTarget } from './bindingRegistry'
    import type { BindingOutcome, BindingTarget, SyncBindingOptions } from './bindingFlow'

    let { target, label, options, onBound, onError }: {
        target: Exclude<BindingTarget, { kind: 'none' }>
        label: string
        options?: SyncBindingOptions
        onBound: (outcome: BindingOutcome) => void
        onError: (error: unknown) => void
    } = $props()
    let busy = $state(false)
    async function connect() {
        if (busy) return
        busy = true
        try { onBound(await (options === undefined ? bindSyncTarget(target) : bindSyncTarget(target, options))) }
        catch (error) { onError(error) }
        finally { busy = false }
    }
</script>

<button type="button" class="rounded border border-borderc bg-darkbutton px-3 py-2 text-textcolor disabled:opacity-50" disabled={busy} onclick={connect}>{label}</button>
