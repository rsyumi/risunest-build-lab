<script lang="ts">
    import { onDestroy } from 'svelte'
    import { language } from 'src/lang'
    import { DBState } from 'src/ts/stores.svelte'
    import type { ChatMessageOverflowScope } from 'src/ts/storage/database.svelte'
    import { getDeviceSettings, subscribeDeviceSettings, updateDeviceSettings } from 'src/ts/storage/deviceSettings'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SettingToggle from '../RisuNest/SettingToggle.svelte'
    import SegmentedButtons from '../RisuNest/SegmentedButtons.svelte'
    import NumberInput from 'src/lib/UI/GUI/NumberInput.svelte'

    type Profile = ReturnType<typeof getDeviceSettings>['performanceProfile']

    const initialSettings = getDeviceSettings()
    let profile = $state(initialSettings.performanceProfile)
    let historyLimitEnabled = $state(initialSettings.generationHistoryLimitEnabled)
    let storedHistoryLimitMultiplier = initialSettings.generationHistoryLimitMultiplier
    let historyLimitMultiplier = $state(initialSettings.generationHistoryLimitMultiplier)
    const unsubscribe = subscribeDeviceSettings((settings) => {
        profile = settings.performanceProfile
        historyLimitEnabled = settings.generationHistoryLimitEnabled
        storedHistoryLimitMultiplier = settings.generationHistoryLimitMultiplier
        historyLimitMultiplier = settings.generationHistoryLimitMultiplier
    })

    onDestroy(unsubscribe)

    const options: { value: Profile; label: string }[] = [
        { value: 'normal', label: language.risuNest.perf.profileNormal },
        { value: 'low-spec', label: language.risuNest.perf.profileLowSpec },
    ]

    const overflowOptions: { value: ChatMessageOverflowScope; label: string }[] = [
        { value: 'latest', label: language.risuNest.perf.overflowScopeLatest },
        { value: 'all', label: language.risuNest.perf.overflowScopeAll },
    ]

    function selectProfile(nextProfile: Profile) {
        if (nextProfile === profile) return
        profile = nextProfile
        updateDeviceSettings({
            performanceProfile: nextProfile,
        })
    }

    function setHistoryLimit(next: boolean) {
        if (next === historyLimitEnabled) return
        historyLimitEnabled = next
        updateDeviceSettings({ generationHistoryLimitEnabled: next })
    }

    function commitHistoryLimitMultiplier() {
        const value = Number(historyLimitMultiplier)
        if (historyLimitMultiplier === null || !Number.isFinite(value)) {
            historyLimitMultiplier = storedHistoryLimitMultiplier
            return
        }
        const next = Math.max(1, value)
        historyLimitMultiplier = next
        if (next === storedHistoryLimitMultiplier) return
        storedHistoryLimitMultiplier = next
        updateDeviceSettings({ generationHistoryLimitMultiplier: next })
    }

    function selectOverflowScope(scope: ChatMessageOverflowScope) {
        if (scope === (DBState.db.chatMessageOverflowScope ?? 'latest')) return
        DBState.db.chatMessageOverflowScope = scope
    }
</script>

<SettingGroup id="risunest-perf" title={language.risuNest.perf.title}>
    <SettingRow label={language.risuNest.perf.profile} help={language.risuNest.perf.profileHelp}>
        <SegmentedButtons value={profile} {options} label={language.risuNest.perf.profile} onchange={selectProfile} />
    </SettingRow>
    <SettingRow label={language.risuNest.perf.overflowScope} help={language.risuNest.perf.overflowScopeHelp}>
        <SegmentedButtons
            value={DBState.db.chatMessageOverflowScope ?? 'latest'}
            options={overflowOptions}
            label={language.risuNest.perf.overflowScope}
            onchange={selectOverflowScope}
        />
    </SettingRow>
    <SettingRow inline label={language.risuNest.perf.historyLimit} help={language.risuNest.perf.historyLimitHelp}>
        <SettingToggle checked={historyLimitEnabled} onchange={setHistoryLimit} label={language.risuNest.perf.historyLimit} />
    </SettingRow>
    {#if historyLimitEnabled}
        <SettingRow label={language.risuNest.perf.historyLimitMultiplier} help={language.risuNest.perf.historyLimitMultiplierHelp}>
            <NumberInput
                size="sm"
                min={1}
                ariaLabel={language.risuNest.perf.historyLimitMultiplier}
                className="w-28 text-right"
                bind:value={historyLimitMultiplier}
                onChange={commitHistoryLimitMultiplier}
            />
        </SettingRow>
    {/if}
</SettingGroup>
