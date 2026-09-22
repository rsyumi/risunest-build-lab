<script lang="ts">
    import { ChevronDownIcon } from '@lucide/svelte'
    import { DBState } from 'src/ts/stores.svelte'
    import { language } from 'src/lang'
    import ModelList from '../UI/ModelList.svelte'
    import SwitchInput from '../UI/GUI/SwitchInput.svelte'
    let expanded = $state(false)
    const auxModels = [
        ['memory', language.axModelMemory],
        ['translate', language.axModelTranslate],
        ['emotion', language.axModelEmotion],
        ['otherAx', language.axModelOther],
    ] as const
</script>

<div class="flex flex-col gap-1 w-full">
    <div class="text-xs text-textcolor2 px-0.5">{language.model}/{language.submodel}</div>
    <div class="model-trigger"><ModelList compact bind:value={DBState.db.aiModel} /></div>
    <div class="flex gap-1 items-stretch">
        <div class="model-trigger min-w-0 flex-1"><ModelList compact bind:value={DBState.db.subModel} /></div>
        <button
            class="shrink-0 px-2 rounded-md border border-darkborderc bg-darkbutton hover:bg-selected transition-colors"
            title={language.seperateModelsForAxModels}
            aria-expanded={expanded}
            onclick={() => {
                expanded = !expanded
            }}
            ><ChevronDownIcon size={16} class="transition-transform {expanded ? 'rotate-180' : ''}" /></button
        >
    </div>
    {#if expanded}
        <div class="flex flex-col gap-1 pl-2 border-l border-selected">
            <SwitchInput
                name={language.seperateModelsForAxModels}
                bind:check={DBState.db.seperateModelsForAxModels}
            />
            {#each auxModels as [key, label]}
                <span class="text-xs text-textcolor2 px-0.5">{label}</span>
                <div class="model-trigger">
                    <ModelList
                        compact
                        blankable
                        disabled={!DBState.db.seperateModelsForAxModels}
                        value={DBState.db.seperateModels?.[key] ?? ''}
                        onChange={(value) => {
                            DBState.db.seperateModels ??= { memory: '', translate: '', emotion: '', otherAx: '' }
                            DBState.db.seperateModels[key] = value
                        }}
                    />
                </div>
            {/each}
        </div>
    {/if}
</div>

<style>
    /* ModelList takes no class, so the compact trigger gets the chevron's hover here. */
    .model-trigger > :global(button) {
        transition: background-color 150ms;
    }

    .model-trigger > :global(button:hover:not(:disabled)) {
        background-color: var(--risu-theme-selected);
    }
</style>
