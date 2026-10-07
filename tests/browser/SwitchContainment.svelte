<script lang="ts">
    import SwitchInput from '../../src/lib/UI/GUI/SwitchInput.svelte'
    import SettingToggle from '../../src/lib/Setting/RisuNest/SettingToggle.svelte'

    let { kind }: { kind: 'switch' | 'setting' } = $props()
    const rows = Array.from({ length: 80 }, (_, index) => index)
</script>
<section>
    {#if kind === 'switch'}
        {#each rows as row}
            <SwitchInput name={`Synthetic switch ${row}`} />
        {/each}
    {:else}
        <div class="flex flex-col">
            {#each rows as row}
                <SettingToggle label={`Synthetic setting ${row}`} showLabel />
            {/each}
        </div>
    {/if}
</section>
