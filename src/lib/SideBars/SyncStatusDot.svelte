<script lang="ts">
    import type { SyncStatusTone } from 'src/ts/storage/sync/syncStatusView'

    /** `class` places the dot and sets the ring that separates it from the background behind it. */
    let { tone, class: className = '' }: { tone: SyncStatusTone; class?: string } = $props()

    const tones: Record<SyncStatusTone, string> = {
        connected: 'bg-success-500',
        working: 'bg-primary-500 motion-safe:animate-pulse',
        idle: 'bg-textcolor/40',
        paused: 'border-[1.5px] border-textcolor2',
        failed: 'bg-textcolor2',
        attention: 'bg-danger-400',
    }
</script>

<span aria-hidden="true" class="pointer-events-none absolute size-[9px] rounded-full ring-2 {tones[tone]} {className}" data-sync-dot={tone}></span>
