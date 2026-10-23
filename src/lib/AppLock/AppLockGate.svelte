<script lang="ts">
    import { tick } from 'svelte'
    import { language } from 'src/lang'
    import { MobileGUI } from 'src/ts/stores.svelte'
    import { appLock } from 'src/ts/appLock/appLock.svelte'
    import AppLockScreen from './AppLockScreen.svelte'
    import AppLockCover from './AppLockCover.svelte'

    interface Props {
        onSurface?: (surface: { element: HTMLElement; keydown: (event: KeyboardEvent) => void } | null) => void
    }
    let { onSurface }: Props = $props()
    const state = $derived(appLock.state)
    const text = $derived(language.risuNest.appLock)
    const method = $derived(state.availability.status === 'available' ? state.availability.method : 'device')
    const nativeAction = $derived(state.availability.status === 'available' ? (state.record.nativeUnlock ? 'unlock' : 'reset') : null)

    $effect(() => {
        const epoch = state.epoch
        const flow = state.flow
        const pending = state.returnPending
        const readFailed = state.readFailed
        const revision = state.presentationRevision
        if (!appLock.gated || !state.initialized) return
        let cancelled = false
        void tick().then(() => requestAnimationFrame(() => requestAnimationFrame(() => {
            if (!cancelled && state.epoch === epoch && state.flow === flow && state.returnPending === pending && state.readFailed === readFailed && state.presentationRevision === revision) {
                void appLock.rendered(text.promptUnlock)
            }
        })))
        return () => { cancelled = true }
    })
</script>

{#if appLock.gated}
    {#if state.readFailed || state.returnPending || !state.flow}
        <AppLockCover readFailed={state.readFailed} busy={state.busy || state.nativeBusy}
            canReset={state.readFailed && state.failure === 'record' && state.availability.status === 'available'}
            onRetry={() => void appLock.retryRead()} onReset={() => void appLock.resetPin(text.resetPin)} {onSurface} />
    {:else}
        <AppLockScreen mode={state.flow} phase={state.phase} count={state.digits.length} compact={!$MobileGUI}
            busy={state.busy} nativeBusy={state.nativeBusy} issue={state.issue} saveFailed={state.saveFailed}
            {nativeAction} nativeMethod={method}
            onDigit={digit => void appLock.digit(digit)} onDelete={() => appLock.deleteDigit()}
            onNative={() => void appLock.unlockWithDevice(text.promptUnlock)} onReset={() => void appLock.resetPin(text.resetPin)}
            onRetry={() => void appLock.retrySave()} onCancel={() => appLock.cancelFlow()} {onSurface} />
    {/if}
{/if}