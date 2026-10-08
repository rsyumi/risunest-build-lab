<script lang="ts">
    import { language } from 'src/lang'
    import { getStartupExclusions, updateStartupExclusions } from 'src/ts/storage/deviceSettings'
    import { exclusionName } from 'src/ts/storage/recoveryExclusionPrompt'
    import type { RecoveryExclusion } from 'src/ts/storage/startupExclusions'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SettingButton from '../RisuNest/SettingButton.svelte'

    const strings = $derived(language.risuNest.recovery)
    let kept = $state(getStartupExclusions())

    function turnOn(exclusion: RecoveryExclusion): void {
        updateStartupExclusions(getStartupExclusions().filter(item => item !== exclusion))
        kept = getStartupExclusions()
    }
</script>

{#if kept.length > 0}
    <SettingGroup title={strings.keptTitle} description={strings.keptHelp} id="risunest-startup-exclusions">
        {#each kept as exclusion (exclusion)}
            <SettingRow label={exclusionName(exclusion)} inline>
                <SettingButton onclick={() => turnOn(exclusion)}>{strings.turnOn}</SettingButton>
            </SettingRow>
        {/each}
    </SettingGroup>
{/if}
