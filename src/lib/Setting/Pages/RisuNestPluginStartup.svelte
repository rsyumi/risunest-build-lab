<script lang="ts">
    import { language } from 'src/lang'
    import { alertError } from 'src/ts/alert'
    import { delayPluginLoadingUntilSync, setDelayPluginLoadingUntilSync } from 'src/ts/plugins/pluginStartupGate'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SettingToggle from '../RisuNest/SettingToggle.svelte'

    let checked = $state(delayPluginLoadingUntilSync())
    let busy = $state(false)
    const copy = language.risuNest.pluginStartup
    async function change(value: boolean) {
        busy = true
        try { await setDelayPluginLoadingUntilSync(value); checked = value }
        catch (error) { checked = delayPluginLoadingUntilSync(); alertError(String(error)) }
        finally { busy = false }
    }
</script>

<SettingGroup title={language.plugin}>
    <SettingRow label={copy.delayLabel} help={copy.delayHelp} labelFor="plugin-sync-delay" inline>
        <SettingToggle id="plugin-sync-delay" label={copy.delayLabel} {checked} disabled={busy} onchange={change} />
    </SettingRow>
</SettingGroup>
