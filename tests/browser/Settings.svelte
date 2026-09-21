<script lang="ts">
    import SettingToggle from '../../src/lib/Setting/RisuNest/SettingToggle.svelte'
    import SegmentedButtons from '../../src/lib/Setting/RisuNest/SegmentedButtons.svelte'
    let checked = $state(false)
    let choice = $state('first')
</script>
<SettingToggle label="Synthetic setting" bind:checked showLabel />
<SegmentedButtons label="Synthetic choice" bind:value={choice} options={[{ value: 'first', label: 'First' }, { value: 'second', label: 'Second' }]} />
<output>{checked ? 'enabled' : 'disabled'}:{choice}</output>
