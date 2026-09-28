<script lang="ts">
    import { onDestroy } from 'svelte'
    import { language } from 'src/lang'
    import { DBState } from 'src/ts/stores.svelte'
    import type { ChatMessageOverflowScope } from 'src/ts/storage/database.svelte'
    import { getDeviceSettings, subscribeDeviceSettings, updateDeviceSettings } from 'src/ts/storage/deviceSettings'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SegmentedButtons from '../RisuNest/SegmentedButtons.svelte'

    type Profile = ReturnType<typeof getDeviceSettings>['performanceProfile']

    let profile = $state(getDeviceSettings().performanceProfile)
    const unsubscribe = subscribeDeviceSettings((settings) => {
        profile = settings.performanceProfile
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
</SettingGroup>
