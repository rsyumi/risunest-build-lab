<script lang="ts">
    import { untrack } from 'svelte'
    import { getModuleToggles } from "src/ts/process/modules";
    import { DBState, selectedCharID } from "src/ts/stores.svelte";
    import { parseToggleSyntax, type sidebarToggle, type sidebarToggleGroup } from "src/ts/util";
    import { language } from "src/lang";
    import type { PromptItem } from "src/ts/process/prompt";
    import { getCurrentCharacter, getCurrentChat, type character } from "src/ts/storage/database.svelte";
    import Accordion from '../UI/Accordion.svelte'
    import SwitchInput from "../UI/GUI/SwitchInput.svelte";
    import SelectInput from "../UI/GUI/SelectInput.svelte";
    import OptionInput from "../UI/GUI/OptionInput.svelte";
    import TextAreaInput from '../UI/GUI/TextAreaInput.svelte'
    import TextInput from "../UI/GUI/TextInput.svelte";
    import ModelBind from './ModelBind.svelte'
    import PersonaBind from './PersonaBind.svelte'
    import ToggleBind from './ToggleBind.svelte'
    import CustomSideBar from "./CustomSidebar.svelte";
    import { getGlobalChatVar, isLocallyHandledGlobalChatVar } from "src/ts/parser/chatVar.svelte";
    import { applyGlobalToggleValues, applyLocalToggleValues, removeLocalToggleValue, setCharacterMemory, setLocalToggleMode, setToggleValue } from 'src/ts/sidebarToggles'
    import { applyToggleValues, snapshotToggleValues, toggleValueChanged, type ToggleValues } from 'src/ts/toggleBindings'
    import { PinIcon } from "@lucide/svelte";
    import { doingChat } from 'src/ts/process/generationState'

    interface Props {
        chara?: character
        noContainer?: boolean
    }

    let { chara = $bindable(), noContainer }: Props = $props();

    const jailbreakToggleToken = '{{jbtoggled}}'
    const usesJailbreakToggle = (value?: string) =>
        typeof value === 'string' && value.includes(jailbreakToggleToken)
    const templateUsesJailbreakToggle = (template: PromptItem[]) =>
        template.some(item => {
            if (item.type === 'jailbreak') {
                return true
            }
            if ('text' in item && usesJailbreakToggle(item.text)) {
                // plain, jailbreak, cot
                return true
            }
            if ('innerFormat' in item && usesJailbreakToggle(item.innerFormat)) {
                // persona, description, lorebook, postEverything, memory
                return true
            }
            if ('defaultText' in item && usesJailbreakToggle(item.defaultText)) {
                // author note
                return true
            }
            return false
        })

    let hasJailbreakPrompt = $derived.by(() => {
        const template = DBState.db.promptTemplate
        if (!template) {
            return (DBState.db.jailbreak ?? '').trim().length > 0
        }
        return templateUsesJailbreakToggle(template)
    })

    // let getToggleDisplayName = (toggle: sidebarToggle) => {
    //     if(isLocallyHandledGlobalChatVar(`toggle_${toggle.key}`)){
    //         return toggle.value + ' (📌)'
    //     }
    //     return toggle.value
    // }
    let charToggle = $state((DBState.db?.characters?.[$selectedCharID] as character)?.customModuleToggle)
    $effect(() => {
        const charToggleTemp = (DBState.db?.characters?.[$selectedCharID] as character)?.customModuleToggle
        if(charToggleTemp !== charToggle) {
            charToggle = charToggleTemp
        }
    })

    let groupedToggles = $derived.by(() => {
        const ungrouped = parseToggleSyntax(
            DBState.db.customPromptTemplateToggle + '\n' +
            getModuleToggles() + '\n' +
            charToggle
        )

        let groupOpen = false
        // group toggles together between group ... groupEnd
        return ungrouped.reduce<sidebarToggle[]>((acc, toggle) => {
            if (toggle.type === 'group') {
                groupOpen = true
                acc.push(toggle)
            } else if (toggle.type === 'groupEnd') {
                groupOpen = false
            } else if (groupOpen) {
                (acc.at(-1) as sidebarToggleGroup).children.push(toggle)
            } else {
                acc.push(toggle)
            }
            return acc
        }, [])
    })

    // Values that differ from the chat's toggle binding are tinted so the user can see what the
    // save button would write. Nothing is tinted while binding is temporarily disabled.
    let savedToggles = $derived.by(() => {
        if (DBState.db.disableToggleBinding) return undefined
        const character = DBState.db.characters[$selectedCharID]
        return character?.chats[character.chatPage]?.savedToggleValues
    })
    let savingToggles = $state(false)
    // Unapplied input for a bound chat: edited keys (null removes one) and local overrides to remove.
    // A binding, selection or local-mode change discards it; replacing the underlying objects does not.
    let toggleDraftScope = $derived.by(() => {
        const character = DBState.db.characters[$selectedCharID]
        const chat = character?.chats[character.chatPage]
        return JSON.stringify([
            character?.chaId, chat?.id ?? character?.chatPage, savedToggles !== undefined, !!chat?.useLocallySetGlobalVariables,
        ])
    })
    let toggleDraft = $derived.by(() => {
        void toggleDraftScope
        return { edits: {} as Record<string, string | null>, unpinKeys: [] as string[] }
    })
    let toggleEditor = $derived.by(() => {
        const values = snapshotToggleValues(DBState.db.globalChatVariables)
        for (const [key, value] of Object.entries(toggleDraft.edits)) {
            if (value === null) delete values[key]
            else values[key] = value
        }
        return { values, unpinKeys: toggleDraft.unpinKeys }
    })
    // Input that the applied values already hold, such as after saving, is no longer an edit.
    $effect.pre(() => {
        const applied = snapshotToggleValues(DBState.db.globalChatVariables)
        const character = DBState.db.characters[$selectedCharID]
        const overrides = { ...character?.chats[character.chatPage]?.GLGlobalVariables }
        untrack(() => {
            const edits = Object.entries(toggleDraft.edits).filter(([key, value]) => value !== (applied[key] ?? null))
            const unpinKeys = toggleDraft.unpinKeys.filter((key) => overrides[key] !== undefined)
            if (edits.length !== Object.keys(toggleDraft.edits).length || unpinKeys.length !== toggleDraft.unpinKeys.length) {
                toggleDraft = { edits: Object.fromEntries(edits), unpinKeys }
            }
        })
    })
    const stageToggleValues = (values: ToggleValues, keys: readonly string[]) => {
        const applied = snapshotToggleValues(DBState.db.globalChatVariables)
        const chat = getCurrentChat()
        // Outside local mode a new value replaces the chat's local override, as setToggleValue does.
        const overrides = chat?.useLocallySetGlobalVariables ? undefined : chat?.GLGlobalVariables
        const edits = { ...toggleDraft.edits }
        const unpinKeys = new Set(toggleDraft.unpinKeys)
        for (const key of keys) {
            if ((values[key] ?? null) === (applied[key] ?? null)) delete edits[key]
            else edits[key] = values[key] ?? null
            if (overrides?.[key] !== undefined) unpinKeys.add(key)
        }
        toggleDraft = { edits, unpinKeys: [...unpinKeys] }
    }
    const editToggle = (key: string, value: string) => {
        if (savingToggles) return
        const chat = getCurrentChat()
        if (savedToggles === undefined || chat?.useLocallySetGlobalVariables) {
            void setToggleValue(key, value)
            return
        }
        stageToggleValues({ [key]: value }, [key])
    }
    const applyTogglePreset = async (values: ToggleValues, keys: readonly string[]): Promise<boolean> => {
        if (savingToggles) return false
        if (getCurrentChat()?.useLocallySetGlobalVariables) return applyLocalToggleValues(values, keys)
        if (savedToggles === undefined) return applyGlobalToggleValues(values, keys)
        const next = { ...toggleEditor.values }
        applyToggleValues(next, values, keys)
        stageToggleValues(next, [...keys, ...Object.keys(values)].filter((key) => key.startsWith('toggle_')))
        return true
    }
    const isToggleDirty = (key: string | undefined) =>
        savedToggles !== undefined &&
        (toggleEditor.unpinKeys.includes(`toggle_${key}`)
            || toggleValueChanged(toggleEditor.values[`toggle_${key}`], savedToggles[`toggle_${key}`]))
    const dirtyClass = (key: string | undefined) => (isToggleDirty(key) ? 'bg-draculared/15' : '')

    // Switch rows sit in the same list as the select/text rows below, which set their own spacing.
    const switchRow = 'mt-2 px-1'

    const getGlobalChatVarNH = (key: string) => {
        const preview = savedToggles !== undefined && (
            toggleEditor.unpinKeys.includes(key) || !isLocallyHandledGlobalChatVar(key)
        )
        const value = preview ? (toggleEditor.values[key] ?? 'null') : getGlobalChatVar(key)
        if (value === 'null') {
            return ''
        }
        return value
    }
    const localEditBlocked = (key: string) => savingToggles || ($doingChat && (
        getCurrentChat()?.useLocallySetGlobalVariables
        || getCurrentChat()?.GLGlobalVariables?.[key] !== undefined
    ))
</script>

{#snippet localToggle(toggle: sidebarToggle)}
    {#if isLocallyHandledGlobalChatVar(`toggle_${toggle.key}`)}
        <button
            disabled={$doingChat || savingToggles}
            onclick={() => {
                void removeLocalToggleValue(`toggle_${toggle.key}`)
            }}
        >
            📌
        </button>
    {/if}
{/snippet}

{#snippet getToggleDisplayName(toggle: sidebarToggle)}
    {toggle.value}{@render localToggle(toggle)}
{/snippet}

{#snippet toggles(items: sidebarToggle[], reverse: boolean = false)}
    {#each items as toggle, index}
        {#if index > 0 && toggle.type !== 'divider' && items[index - 1]?.type !== 'divider' && toggle.type !== 'caption' && items[index - 1]?.type !== 'caption' && !(toggle.type === 'group' && items[index - 1]?.type === 'group')}
            <div class="w-full my-2 border-t border-darkborderc/20"></div>
        {/if}
        {#if toggle.type === 'group' && toggle.children.length > 0}
            <div class="w-full mt-1">
                <Accordion styled name={toggle.value}>
                    {@render toggles((toggle as sidebarToggleGroup).children, reverse)}
                </Accordion>
            </div>
        {:else if toggle.type === 'select'}
            <div inert={localEditBlocked(`toggle_${toggle.key}`)} class="w-full flex gap-2 mt-2 items-center justify-between min-h-10 rounded-md px-1 transition-colors {dirtyClass(toggle.key)}">
                <span class="min-w-0 break-words">{@render getToggleDisplayName(toggle)}</span>
                <SelectInput
                    className="w-32 shrink-0"
                    value={getGlobalChatVarNH(`toggle_${toggle.key}`)}
                    onchange={(e) => {
                        editToggle(`toggle_${toggle.key}`, e.currentTarget.value)
                    }}
                >
                    {#each toggle.options as option, i}
                        <OptionInput value={i.toString()}>{option}</OptionInput>
                    {/each}
                </SelectInput>
            </div>
        {:else if toggle.type === 'text'}
            <div class="w-full flex gap-2 mt-2 items-center justify-between min-h-10 rounded-md px-1 transition-colors {dirtyClass(toggle.key)}">
                <span class="min-w-0 break-words">{@render getToggleDisplayName(toggle)}</span>
                <TextInput
                    className="w-32 shrink-0"
                    disabled={localEditBlocked(`toggle_${toggle.key}`)}
                    value={getGlobalChatVarNH(`toggle_${toggle.key}`)}
                    onchange={(e) => {
                        editToggle(`toggle_${toggle.key}`, e.currentTarget.value)
                    }}
                />
            </div>
        {:else if toggle.type === 'textarea'}
            <div inert={localEditBlocked(`toggle_${toggle.key}`)} class="w-full flex gap-2 mt-2 items-start justify-between min-h-10 rounded-md px-1 transition-colors {dirtyClass(toggle.key)}">
                <span class="min-w-0 break-words mt-1.5">{@render getToggleDisplayName(toggle)}</span>
                <TextAreaInput
                    className="w-32 shrink-0"
                    height="20"
                    value={getGlobalChatVarNH(`toggle_${toggle.key}`)}
                    onchange={(e) => {
                        //check is div
                        if (e.currentTarget instanceof HTMLDivElement) {
                            editToggle(`toggle_${toggle.key}`, e.currentTarget.innerText)
                        } else {
                            editToggle(`toggle_${toggle.key}`, e.currentTarget.value)
                        }
                    }}
                />
            </div>
        {:else if toggle.type === 'caption'}
            <div class="w-full mt-1 text-xs text-textcolor2">
                {toggle.value}
            </div>
        {:else if toggle.type === 'divider'}
            <!-- Prevent multiple dividers appearing in a row -->
            {#if index === 0 || items[index - 1]?.type !== 'divider' || items[index - 1]?.value !== toggle.value}
                <div class="w-full min-h-5 flex gap-2 mt-2 items-center" class:justify-end={!reverse}>
                    {#if toggle.value}
                        <span class="shrink-0">{@render getToggleDisplayName(toggle)}</span>
                    {/if}
                    <hr class="border-t border-darkborderc m-0 grow" />
                </div>
            {/if}
        {:else}
            <SwitchInput
                className={switchRow}
                disabled={localEditBlocked(`toggle_${toggle.key}`)}
                check={getGlobalChatVarNH(`toggle_${toggle.key}`) === '1'}
                name={toggle.value}
                highlight={isToggleDirty(toggle.key)}
                onChange={(checked) => {
                    editToggle(`toggle_${toggle.key}`, checked ? '1' : '0')
                }}
            >
                {@render localToggle(toggle)}
            </SwitchInput>
        {/if}
    {/each}
{/snippet}

<div class="flex flex-col gap-4 w-full mt-3">
    {#if !DBState.db.customSidebarItems?.some((item) => item.type === 'model')}<ModelBind />{/if}
    {#if !DBState.db.customSidebarItems?.some((item) => item.type === 'persona')}<PersonaBind />{/if}
    {#if DBState.db.customSidebarItems?.length}<CustomSideBar />{/if}
    <ToggleBind values={toggleEditor.values} unpinKeys={toggleEditor.unpinKeys} onApplyPreset={applyTogglePreset} bind:saving={savingToggles} />
</div>
{#if !noContainer && groupedToggles.length > 4}
    <div class="h-48 border-darkborderc p-2 border rounded-sm flex flex-col items-start mt-2 overflow-y-auto">
        {#if hasJailbreakPrompt}
            <SwitchInput className={switchRow} bind:check={DBState.db.jailbreakToggle} name={language.jailbreakToggle} />
        {/if}

        {@render toggles(groupedToggles, true)}
        {#if chara && (DBState.db.supaModelType !== 'none' || DBState.db.hanuraiEnable || DBState.db.hypaV3)}
            <SwitchInput
                className={switchRow}
                check={chara.supaMemory}
                onChange={(checked) => void setCharacterMemory(chara, checked)}
                name={DBState.db.hypaV3
                    ? language.ToggleHypaMemory
                    : DBState.db.hanuraiEnable
                      ? language.hanuraiMemory
                      : DBState.db.hypaMemory
                        ? language.ToggleHypaMemory
                        : language.ToggleSuperMemory}
            />
        {/if}
    </div>
{:else}
    {#if hasJailbreakPrompt}
        <SwitchInput className={switchRow} bind:check={DBState.db.jailbreakToggle} name={language.jailbreakToggle} />
    {/if}
    {@render toggles(groupedToggles)}
    {#if chara && (DBState.db.supaModelType !== 'none' || DBState.db.hanuraiEnable || DBState.db.hypaV3)}
        <SwitchInput
            className={switchRow}
            check={chara.supaMemory}
            onChange={(checked) => void setCharacterMemory(chara, checked)}
            name={DBState.db.hypaV3
                ? language.ToggleHypaMemory
                : DBState.db.hanuraiEnable
                  ? language.hanuraiMemory
                  : DBState.db.hypaMemory
                    ? language.ToggleHypaMemory
                    : language.ToggleSuperMemory}
        />
    {/if}

    {#if chara}
        <SwitchInput
            className={switchRow}
            check={getCurrentChat()?.useLocallySetGlobalVariables}
            disabled={$doingChat || savingToggles}
            name={language.localToggles}
            onChange={(checked) => void setLocalToggleMode(checked)}
        />
    {/if}
{/if}
