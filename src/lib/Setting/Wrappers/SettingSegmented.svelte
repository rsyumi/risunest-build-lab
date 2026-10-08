<script lang="ts">
    import type { SettingItem, SettingContext } from 'src/ts/setting/types';
    import { UNINITIALIZED, getLabel, getSettingValue, resolveLanguagePath, setSettingValue } from 'src/ts/setting/utils';
    import { untrack } from 'svelte';
    import SegmentedControl from 'src/lib/UI/GUI/SegmentedControl.svelte';
    import Help from 'src/lib/Others/Help.svelte';

    interface Props {
        item: SettingItem;
        ctx: SettingContext;
    }

    let { item, ctx }: Props = $props();

    let localValue: any = $state(untrack(() => getSettingValue(item, ctx)));

    $effect(() => {
        localValue = getSettingValue(item, ctx);
    });

    $effect(() => item.subscribeValue?.(() => {
        localValue = getSettingValue(item, ctx);
    }));

    $effect(() => {
        const val = localValue;
        if (val === UNINITIALIZED) return;
        untrack(() => {
            if (val !== getSettingValue(item, ctx)) {
                setSettingValue(item, val, ctx);
            }
        });
    });

    // Transform options: filter by condition + resolve labelKey translations
    let processedOptions = $derived((item.options?.segmentOptions ?? [])
        .filter(opt => !opt.condition || opt.condition(ctx))
        .map(opt => {
            const translated = opt.labelKey ? resolveLanguagePath(opt.labelKey) : undefined;
            return {
                value: opt.value,
                label: typeof translated === 'string' ? translated : (opt.label ?? '')
            };
        }));

    // Reset value if current selection becomes hidden due to condition changes
    $effect(() => {
        const currentVal = getSettingValue(item, ctx);
        if (processedOptions.length > 0 && currentVal !== undefined && !processedOptions.some(o => o.value === currentVal)) {
            const numericOptions = processedOptions.filter((o): o is { value: number; label: string } => typeof o.value === 'number');
            const fallback = typeof currentVal === 'number' && numericOptions.length > 0
                ? numericOptions.reduce((closest, option) => Math.abs(option.value - currentVal) < Math.abs(closest.value - currentVal) ? option : closest).value
                : processedOptions[processedOptions.length - 1].value;
            setSettingValue(item, fallback, ctx);
        }
    });
</script>

<span class="text-textcolor {item.classes ?? ''}">
    {getLabel(item)}
    {#if item.showExperimental}<Help key="experimental"/>{/if}
    {#if item.helpKey}<Help key={item.helpKey as any} unrecommended={item.helpUnrecommended ?? false}/>{/if}
</span>
<SegmentedControl
    bind:value={localValue}
    options={processedOptions}
/>
