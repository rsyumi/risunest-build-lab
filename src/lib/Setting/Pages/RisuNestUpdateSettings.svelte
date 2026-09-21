<script lang="ts">
    import { onMount } from 'svelte'
    import { language } from 'src/lang'
    import SettingButton from '../RisuNest/SettingButton.svelte'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SettingToggle from '../RisuNest/SettingToggle.svelte'
    import { appUpdateState } from 'src/ts/update/state.svelte'
    import { checkForAppUpdate, clearSkippedAppUpdate } from 'src/ts/update/controller'
    import {
        getAppUpdateSettings,
        subscribeAppUpdateSettings,
        updateAppUpdateSettings,
        type AppUpdateSettings,
    } from 'src/ts/update/settings'
    import { nativeUpdate } from 'src/ts/update/native'
    import { openURL } from 'src/ts/globalApi.svelte'

    let settings = $state<AppUpdateSettings>({
        schema: 'risunest.app-update-settings/v1',
        autoUpdateCheck: true,
        skippedVersion: '',
        lastCheckedAt: 0,
    })
    let settingsError = $state('')
    const text = $derived(language.risuNest.update)

    onMount(() => {
        try {
            settings = getAppUpdateSettings()
        } catch (error) {
            settingsError = error instanceof Error ? error.message : String(error)
        }
        const unsubscribe = subscribeAppUpdateSettings(next => { settings = next })
        if (!$appUpdateState.environment) {
            void nativeUpdate.environment().then(environment => {
                appUpdateState.update(state => ({ ...state, environment }))
            })
        }
        return unsubscribe
    })

    function setAutomatic(value: boolean): void {
        try {
            settings = updateAppUpdateSettings({ autoUpdateCheck: value })
            settingsError = ''
        } catch (error) {
            settingsError = error instanceof Error ? error.message : String(error)
        }
    }

    function clearSkipped(): void {
        clearSkippedAppUpdate()
        settings = getAppUpdateSettings()
    }

    function checkedAt(value: number): string {
        return value ? new Date(value).toLocaleString() : text.never
    }
</script>

<SettingGroup id="risunest-update" title={text.title} description={text.description}>
    <SettingRow label={text.automatic} help={text.automaticHelp} inline>
        <SettingToggle
            id="risunest-update-automatic"
            label={text.automatic}
            checked={settings.autoUpdateCheck}
            disabled={!!settingsError}
            onchange={setAutomatic} />
    </SettingRow>
    <SettingRow label={text.currentVersion}>
        <span class="text-sm">{$appUpdateState.environment?.currentVersion ?? language.loading}</span>
    </SettingRow>
    <SettingRow label={text.installMethod}>
        <span class="text-sm">{text.strategies[$appUpdateState.environment?.installStrategy ?? 'disabled']}</span>
    </SettingRow>
    <SettingRow label={text.lastChecked}>
        <span class="text-sm">{checkedAt(settings.lastCheckedAt)}</span>
        <SettingButton busy={$appUpdateState.phase === 'checking'} onclick={() => void checkForAppUpdate(true)}>{text.checkNow}</SettingButton>
    </SettingRow>
    {#if settings.skippedVersion}
        <SettingRow label={text.skippedVersion}>
            <span class="text-sm">{settings.skippedVersion}</span>
            <SettingButton variant="secondary" onclick={clearSkipped}>{text.clearSkipped}</SettingButton>
        </SettingRow>
    {/if}
    {#if $appUpdateState.update}
        <SettingRow label={text.releasePage}>
            <SettingButton variant="secondary" onclick={() => openURL($appUpdateState.update!.releasePage)}>{text.openRelease}</SettingButton>
        </SettingRow>
    {/if}
    {#if settingsError}
        <div class="px-4 py-3 text-sm text-danger-400" role="alert">{text.settingsError}: {settingsError}</div>
    {/if}
</SettingGroup>
