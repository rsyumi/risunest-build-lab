<script lang="ts">
    import { language } from 'src/lang'
    import { modalNavigation } from 'src/ts/ui/modalNavigation'
    import type { QrScanTone } from 'src/ts/ui/qrScanner'
    import SettingButton from '../Setting/RisuNest/SettingButton.svelte'

    interface Props {
        oncancel: () => void
        tone: QrScanTone
    }

    let { oncancel, tone }: Props = $props()
    const copy = language.risuNest.qrScan
</script>

<!-- The camera preview sits behind the web view; only the area inside the frame is left uncovered. -->
<div
    class="risunest-qr-scan fixed inset-0 z-modal flex flex-col items-center px-6 pt-[max(1.5rem,env(safe-area-inset-top))] pb-[max(2rem,env(safe-area-inset-bottom))] text-textcolor"
    role="dialog"
    aria-modal="true"
    aria-labelledby="risunest-qr-scan-instruction"
    use:modalNavigation={{ close: oncancel }}
>
    <div class="flex-1"></div>
    <div class="frame" aria-hidden="true"></div>
    <p id="risunest-qr-scan-instruction" class="instruction relative mt-6 max-w-xs rounded-full px-4 py-2 text-center text-sm font-medium">
        {tone === 'onboarding' ? copy.instructionOnboarding : copy.instruction}
    </p>
    <div class="relative flex flex-1 items-end">
        <SettingButton class="min-w-28 py-2" onclick={oncancel}>{language.cancel}</SettingButton>
    </div>
</div>

<style>
    .frame {
        position: relative;
        width: min(72vw, 52vh, 18rem);
        aspect-ratio: 1;
        border: 2px solid var(--risu-theme-textcolor);
        border-radius: 1rem;
    }
    /* Dims everything outside the frame so the text stays readable over any camera image. The
       shadow fades through opacity because older Android WebViews drop a color-mix() shadow color. */
    .frame::before {
        content: '';
        position: absolute;
        inset: -2px;
        border-radius: inherit;
        box-shadow: 0 0 0 100vmax var(--risu-theme-darkbg);
        opacity: 0.72;
    }
    .instruction {
        background: color-mix(in srgb, var(--risu-theme-darkbg) 85%, transparent);
    }
</style>
