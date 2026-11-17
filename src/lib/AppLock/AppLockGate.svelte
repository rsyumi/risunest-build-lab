<script lang="ts">
    import { tick } from 'svelte'
    import { language } from 'src/lang'
    import { MobileGUI } from 'src/ts/stores.svelte'
    import { appLock } from 'src/ts/appLock/appLock.svelte'
    import { captureAppLockPresentationFocus } from 'src/ts/appLock/input'
    import AppLockScreen from './AppLockScreen.svelte'
    import AppLockCover from './AppLockCover.svelte'

    interface Props {
        onSurface?: (surface: { element: HTMLElement; keydown: (event: KeyboardEvent) => void } | null) => void
    }
    let { onSurface }: Props = $props()
    const state = $derived(appLock.state)
    const text = $derived(language.risuNest.appLock)
    const flow = $derived(state.flow)
    const method = $derived(state.availability.status === 'available' ? state.availability.method : 'device')
    const nativeAction = $derived(state.availability.status === 'available' ? (state.record.nativeUnlock ? 'unlock' : 'reset') : null)

    const presentation = $derived(appLock.gated && state.initialized && state.active
        ? `${state.epoch}:${flow}:${state.returnPending}:${state.readFailed}:${state.presentationRevision}` : null)

    $effect(() => {
        const renderedPresentation = presentation
        if (renderedPresentation === null) return
        let cancelled = false
        const current = () => !cancelled && presentation === renderedPresentation
        void tick().then(() => {
            const restoreFocus = captureAppLockPresentationFocus()
            requestAnimationFrame(() => requestAnimationFrame(async () => {
                if (!current()) return
                await appLock.rendered(text.promptUnlock)
                // Removing a native cover can focus the WebView's first button after mount.
                requestAnimationFrame(() => {
                    if (current() && !state.nativeBusy) restoreFocus()
                })
            }))
        })
        return () => { cancelled = true }
    })
</script>

{#if appLock.gated}
    {#if state.readFailed || state.returnPending || !flow}
        <AppLockCover readFailed={state.readFailed} busy={state.busy || state.nativeBusy}
            canReset={state.readFailed && state.failure === 'record' && state.availability.status === 'available'}
            onRetry={() => void appLock.retryRead()} onReset={() => void appLock.resetPin(text.resetPin)} {onSurface} />
    {:else}
        <AppLockScreen mode={flow} phase={state.phase} count={state.digits.length} compact={!$MobileGUI}
            busy={state.busy} nativeBusy={state.nativeBusy} issue={state.issue} saveFailed={state.saveFailed}
            {nativeAction} nativeMethod={method}
            onDigit={digit => void appLock.digit(digit)} onDelete={() => appLock.deleteDigit()}
            onNative={() => void appLock.unlockWithDevice(text.promptUnlock)} onReset={() => void appLock.resetPin(text.resetPin)}
            onRetry={() => void appLock.retrySave()} onCancel={() => appLock.cancelFlow()} {onSurface} />
    {/if}
{/if}