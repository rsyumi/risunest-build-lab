<script lang="ts">
    import {
        IncrementalTextUnits,
        type TextUnit,
    } from '../../ts/parser/boundedTextUnits'

    let { text }: { text: string } = $props()
    let units: readonly TextUnit[] = $state.raw([])
    const mounter = new IncrementalTextUnits((next) => {
        units = next
    })

    $effect.pre(() => {
        mounter.setText(text)
    })
    $effect(() => () => mounter.dispose())
</script>

<!-- Inline-block units bound each layout while selection and copy stay continuous. -->
<span class="incremental-text" data-incremental-plain-text
    >{#each units as unit (unit.start)}<span class="incremental-text-unit"
            >{unit.text}</span
        >{/each}</span
>

<style>
    .incremental-text {
        display: block;
    }
    .incremental-text-unit {
        display: inline-block;
        width: 100%;
        vertical-align: top;
        white-space: pre-wrap;
    }
</style>
