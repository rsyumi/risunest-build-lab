<script lang="ts" generics="T extends string | number">
    interface Props {
        value: T
        options: { value: T; label: string }[]
        /** Accessible name of the whole control. */
        label: string
        /** `group` renders pressed buttons; `radiogroup` renders radio semantics. */
        role?: 'group' | 'radiogroup'
        /** Ignore clicks and dim the control while the owning row is busy. */
        disabled?: boolean
        onchange?: (value: T) => void
    }

    let { value = $bindable(), options, label, role = 'group', disabled = false, onchange }: Props = $props()

    function select(next: T): void {
        if (disabled || next === value) return
        value = next
        onchange?.(next)
    }
</script>

<!-- Options wrap onto a second line instead of clipping when the panel is narrower than their labels. -->
<div
    {role}
    aria-label={label}
    aria-disabled={disabled ? 'true' : undefined}
    class="inline-flex w-full flex-wrap gap-0.5 rounded-lg border border-darkborderc bg-bgcolor p-1 @xl:w-auto {disabled ? 'opacity-50' : ''}"
>
    {#each options as option (option.value)}
        {@const active = option.value === value}
        <button
            type="button"
            role={role === 'radiogroup' ? 'radio' : undefined}
            aria-checked={role === 'radiogroup' ? active : undefined}
            aria-pressed={role === 'group' ? active : undefined}
            {disabled}
            class="flex-1 rounded-md px-3 py-1.5 text-sm whitespace-nowrap transition-colors duration-200 focus:outline-hidden focus-visible:ring-2 focus-visible:ring-selected disabled:cursor-not-allowed @xl:flex-none {active ? 'bg-darkborderc text-textcolor' : 'text-textcolor2 hover:text-textcolor'}"
            onclick={() => select(option.value)}
        >{option.label}</button>
    {/each}
</div>
