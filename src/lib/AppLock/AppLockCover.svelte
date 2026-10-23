<script lang="ts">
    import { onMount } from 'svelte'
    import { language } from 'src/lang'
    import risuNestLogo from '../../../src-tauri/icon-src/logo.svg?url'
    import SettingButton from '../Setting/RisuNest/SettingButton.svelte'

    interface Props {
        readFailed?: boolean
        busy?: boolean
        canReset?: boolean
        onRetry?: () => void
        onReset?: () => void
        onSurface?: (surface: { element: HTMLElement; keydown: (event: KeyboardEvent) => void } | null) => void
    }
    let { readFailed = false, busy = false, canReset = false, onRetry, onReset, onSurface }: Props = $props()
    let root: HTMLDivElement

    function keydown(event: KeyboardEvent): void {
        if (event.key === 'Tab') {
            event.preventDefault()
            const buttons = [...root.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')]
            const current = buttons.indexOf(document.activeElement as HTMLButtonElement)
            const next = current < 0 ? (event.shiftKey ? buttons.length - 1 : 0)
                : (current + (event.shiftKey ? -1 : 1) + buttons.length) % buttons.length
            ;(buttons[next] ?? root).focus()
        } else if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault()
            const target = document.activeElement
            if (target instanceof HTMLButtonElement && root.contains(target) && !target.disabled) target.click()
        } else event.preventDefault()
    }

    onMount(() => {
        root.focus({ preventScroll: true })
        onSurface?.({ element: root, keydown })
        return () => onSurface?.(null)
    })
</script>

<div bind:this={root} class="app-lock-cover fixed inset-0 z-app-lock bg-bgcolor text-textcolor" role="dialog"
    aria-modal="true" aria-label="RisuNest" tabindex="-1" onkeydown={keydown}>
    <img src={risuNestLogo} alt="" />
    {#if readFailed}
        <p role="alert">{language.risuNest.localData.loadFailed}</p>
        <div class="actions">
            <SettingButton {busy} onclick={onRetry}>{language.retry}</SettingButton>
            {#if canReset}<SettingButton variant="quiet" disabled={busy} onclick={onReset}>{language.risuNest.appLock.resetPin}</SettingButton>{/if}
        </div>
    {/if}
</div>

<style>
    .app-lock-cover { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 14px; padding: 24px; outline: none; }
    img { width: 56px; height: 56px; border-radius: 14px; }
    p { margin: 0; font-size: 15px; text-align: center; }
    .actions { display: flex; flex-direction: column; align-items: center; gap: 6px; }
</style>