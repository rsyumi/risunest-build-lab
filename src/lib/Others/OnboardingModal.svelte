<script lang="ts">
    import { CheckIcon, CompassIcon, XIcon } from "@lucide/svelte";
    import { onDestroy, tick, untrack } from "svelte";
    import { language } from "src/lang";
    import { DBState, onboardingModalStore, selectedCharID } from "src/ts/stores.svelte";
    import { ColorSchemeTypeStore } from "src/ts/gui/colorscheme";
    import type { character, OnboardingQuestion } from "src/ts/storage/database.svelte";
    import {
        getActiveOnboarding,
        getOptionValue,
        isQuestionAnswered,
        onboardingAnswerChanges,
        onboardingVarNameAttr,
        onboardingVarValueAttr,
        parseMultipleValue,
        readFormAnswers,
        readOnboardingVar,
        stringifyMultipleValue,
        type OnboardingVarReader,
    } from "src/ts/onboarding";
    import { parseOnboardingHTML } from "src/ts/parser/parser.svelte";
    import { getChatVarFromConversation } from "src/ts/parser/chatVar.svelte";
    import { captureOnboardingTarget, finishOnboarding } from "src/ts/chatBindings.svelte";
    import { DeferredInlayMarkerRegistry, mountDeferredInlaySources } from "src/ts/process/files/inlayRenderSource";
    import { alertError } from "src/ts/alert";
    import { backgroundErrorMessage } from "src/ts/storage/backgroundErrorMessage";
    import Button from "../UI/GUI/Button.svelte";
    import TextInput from "../UI/GUI/TextInput.svelte";

    interface Props {
        /** Shows the creator's draft without touching any chat. */
        preview?: boolean;
        onclose?: () => void;
    }

    let { preview = false, onclose }: Props = $props();

    // The chat Done and Skip write to is fixed when the onboarding opens.
    const target = untrack(() => preview) ? null : captureOnboardingTarget()
    const char = $derived(
        (target
            ? DBState.db.characters.find((candidate) => candidate.chaId === target.characterId)
            : DBState.db.characters[$selectedCharID]) as character | undefined
    )
    const onboarding = $derived(preview ? char?.onboarding : getActiveOnboarding(char))
    const questions = $derived(onboarding?.questions?.filter((q) => q.varName) ?? [])

    const readVar: OnboardingVarReader = (varName) => {
        const conversation = target?.conversation
        return conversation
            ? getChatVarFromConversation(DBState.db, target.characterId, conversation, varName)
            : 'null'
    }

    // Answers are staged here and only written to the chat on Done, so Skip leaves the variables untouched.
    let answers: Record<string, string> = $state({})
    let renderedHTML = $state('')
    let renderEpoch = $state(0)
    let htmlContainer: HTMLDivElement | undefined = $state()
    let showRequiredError = $state(false)
    let saving = $state(false)
    let renderGeneration = 0
    let renderRegistry: DeferredInlayMarkerRegistry | null = null
    let releaseInlays = () => {}
    let destroyed = false

    const missingRequired = $derived(
        onboarding?.mode === 'form' && questions.some((q) => q.required && !isQuestionAnswered(q, answers))
    )

    $effect(() => {
        if(!onboarding){
            return
        }
        const mode = onboarding.mode
        const html = onboarding.html ?? ''
        const currentQuestions = questions
        untrack(() => {
            answers = preview ? {} : readFormAnswers(currentQuestions, readVar)
            if(mode === 'html'){
                void renderHTML(html)
            }
        })
    })

    onDestroy(() => {
        destroyed = true
        releaseInlays()
        renderRegistry?.clear()
    })

    async function renderHTML(html: string){
        const generation = ++renderGeneration
        const owner = char
        if(!owner){
            return
        }
        const registry = new DeferredInlayMarkerRegistry()
        let rendered: string
        try {
            rendered = await parseOnboardingHTML(html, owner, registry)
        } catch (error) {
            registry.clear()
            if(!destroyed && generation === renderGeneration){
                console.error('Onboarding render failed', error)
            }
            return
        }
        if(destroyed || generation !== renderGeneration){
            registry.clear()
            return
        }
        releaseInlays()
        releaseInlays = () => {}
        renderRegistry?.clear()
        renderRegistry = registry
        renderedHTML = rendered
        renderEpoch += 1
        await tick()
        if(destroyed || generation !== renderGeneration){
            return
        }
        if(htmlContainer){
            releaseInlays = mountDeferredInlaySources(htmlContainer, registry)
        }
        syncHTMLState(true)
    }

    function getBoundElements(): HTMLElement[] {
        if(!htmlContainer){
            return []
        }
        return Array.from(htmlContainer.querySelectorAll<HTMLElement>(`[${onboardingVarNameAttr}]`))
    }

    function isFormControl(el: Element): el is HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement {
        return el instanceof HTMLInputElement || el instanceof HTMLSelectElement || el instanceof HTMLTextAreaElement
    }

    function getStagedValue(varName: string): string | undefined {
        if(varName in answers){
            return answers[varName]
        }
        return preview ? undefined : readOnboardingVar(readVar, varName)
    }

    /**
     * Marks clickable choices whose value matches the staged variable with the
     * `selected` class (scoped to `x-risu-selected`, so creators style it with
     * `.selected` in their <style>). When `fillInputs` is set, form controls are
     * also filled from the current values.
     */
    function syncHTMLState(fillInputs: boolean){
        for(const el of getBoundElements()){
            const varName = el.getAttribute(onboardingVarNameAttr)
            const current = getStagedValue(varName)
            if(isFormControl(el)){
                if(!fillInputs || current === undefined){
                    continue
                }
                if(el instanceof HTMLInputElement && (el.type === 'checkbox' || el.type === 'radio')){
                    const onValue = el.getAttribute(onboardingVarValueAttr) ?? (el.type === 'checkbox' ? '1' : el.value)
                    el.checked = current === onValue
                }
                else{
                    el.value = current
                }
                continue
            }
            const selected = current !== undefined && current === (el.getAttribute(onboardingVarValueAttr) ?? '')
            el.classList.toggle('x-risu-selected', selected)
        }
    }

    function onHTMLClick(e: MouseEvent){
        const element = (e.target as Element)?.closest?.(`[${onboardingVarNameAttr}]`)
        if(!element || !htmlContainer?.contains(element) || isFormControl(element)){
            return
        }
        e.preventDefault()
        answers[element.getAttribute(onboardingVarNameAttr)] = element.getAttribute(onboardingVarValueAttr) ?? ''
        syncHTMLState(false)
    }

    function onHTMLInput(e: Event){
        const element = e.target as Element
        if(!isFormControl(element) || !element.hasAttribute(onboardingVarNameAttr)){
            return
        }
        const varName = element.getAttribute(onboardingVarNameAttr)
        const fixedValue = element.getAttribute(onboardingVarValueAttr)
        if(element instanceof HTMLInputElement && element.type === 'checkbox'){
            answers[varName] = element.checked ? (fixedValue ?? '1') : '0'
        }
        else if(element instanceof HTMLInputElement && element.type === 'radio'){
            if(element.checked){
                answers[varName] = fixedValue ?? element.value
            }
        }
        else{
            answers[varName] = element.value
        }
        syncHTMLState(false)
    }

    function selectSingle(question: OnboardingQuestion, value: string){
        answers[question.varName] = value
    }

    function toggleMultiple(question: OnboardingQuestion, value: string){
        const values = parseMultipleValue(answers[question.varName])
        const index = values.indexOf(value)
        if(index === -1){
            values.push(value)
        }
        else{
            values.splice(index, 1)
        }
        answers[question.varName] = stringifyMultipleValue(values)
    }

    function close(){
        if(onclose){
            onclose()
            return
        }
        onboardingModalStore.open = false
    }

    async function apply(changes: Map<string, string> | null){
        if(preview || !target){
            close()
            return
        }
        if(saving){
            return
        }
        saving = true
        try {
            const result = await finishOnboarding(target, changes)
            if(result !== 'blocked'){
                close()
            }
        } catch (error) {
            alertError(backgroundErrorMessage(error))
        } finally {
            saving = false
        }
    }

    function skip(){
        void apply(null)
    }

    function finish(){
        if(missingRequired){
            showRequiredError = true
            return
        }
        void apply(onboardingAnswerChanges($state.snapshot(answers)))
    }
</script>

{#snippet choice(selected: boolean, multiple: boolean, label: string, onclick: () => void)}
    <button
        class="flex items-center gap-3 w-full text-left px-3 py-2.5 rounded-md border transition-colors {selected
            ? 'border-borderc bg-selected text-textcolor'
            : 'border-darkborderc bg-textcolor/5 text-textcolor/80 hover:bg-textcolor/10'}"
        role={multiple ? 'checkbox' : 'radio'}
        aria-checked={selected}
        onclick={onclick}
    >
        <span
            class="shrink-0 w-4 h-4 flex items-center justify-center border transition-colors {multiple ? 'rounded-sm' : 'rounded-full'} {selected
                ? 'border-textcolor bg-textcolor text-bgcolor'
                : 'border-textcolor/40'}"
        >
            {#if selected}
                {#if multiple}
                    <CheckIcon size={12} strokeWidth={3} />
                {:else}
                    <span class="w-1.5 h-1.5 rounded-full bg-bgcolor"></span>
                {/if}
            {/if}
        </span>
        <span class="text-sm break-any">{label}</span>
    </button>
{/snippet}

<div
    class="fixed inset-0 z-modal bg-black/60 flex justify-center items-center p-4"
    role="presentation"
    onclick={(e) => {
        if (e.target === e.currentTarget) close();
    }}
>
    <div class="bg-darkbg rounded-lg flex flex-col w-full max-w-2xl max-h-[90vh] overflow-hidden shadow-xl">
        <div class="flex items-center justify-between gap-2 px-5 py-3 border-b border-textcolor/10 shrink-0">
            <span class="flex items-center gap-2 min-w-0 text-base font-semibold text-textcolor/90">
                <CompassIcon size={18} class="shrink-0" />
                <span class="truncate">{onboarding?.title || char?.name || language.onboarding}</span>
                {#if preview}
                    <span class="shrink-0 px-2 py-0.5 rounded text-xs font-medium bg-textcolor/10 text-textcolor/60">{language.preview}</span>
                {/if}
            </span>
            <button
                class="text-textcolor/50 hover:text-textcolor/90 transition-colors"
                onclick={close}
                aria-label={language.close}
            >
                <XIcon size={18} />
            </button>
        </div>

        <div class="overflow-y-auto flex-1 px-5 py-4 flex flex-col gap-5 break-any">
            {#if !onboarding}
                <p class="text-textcolor/40 text-sm">{language.noData}</p>
            {:else if onboarding.mode === 'html'}
                <!-- svelte-ignore a11y_click_events_have_key_events -->
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div
                    class="chattext prose max-w-full wrap-break-word text-textcolor"
                    class:prose-invert={$ColorSchemeTypeStore}
                    bind:this={htmlContainer}
                    onclick={onHTMLClick}
                    oninput={onHTMLInput}
                    onchange={onHTMLInput}
                    onsubmit={(e) => e.preventDefault()}
                >
                    {#key renderEpoch}
                        {@html renderedHTML}
                    {/key}
                </div>
            {:else}
                {#if onboarding.description}
                    <p class="text-sm text-textcolor/70 whitespace-pre-wrap">{onboarding.description}</p>
                {/if}
                {#each questions as question (question.id)}
                    {@const invalid = showRequiredError && question.required && !isQuestionAnswered(question, answers)}
                    <section class="flex flex-col gap-2">
                        <div>
                            <h3 class="text-sm font-semibold text-textcolor/90">
                                {question.title || question.varName}
                                {#if question.required}
                                    <span class="text-draculared">*</span>
                                {/if}
                            </h3>
                            {#if question.description}
                                <p class="text-xs text-textcolor/50 mt-0.5 whitespace-pre-wrap">{question.description}</p>
                            {/if}
                        </div>
                        {#if question.type === 'text'}
                            <TextInput
                                size="sm"
                                fullwidth
                                placeholder={question.placeholder ?? ''}
                                value={answers[question.varName] ?? ''}
                                oninput={(e) => { answers[question.varName] = (e.currentTarget as HTMLInputElement).value }}
                            />
                        {:else}
                            <div class="flex flex-col gap-1.5" role={question.type === 'single' ? 'radiogroup' : 'group'}>
                                {#each question.options as option}
                                    {@const value = getOptionValue(option)}
                                    {#if question.type === 'multiple'}
                                        {@render choice(parseMultipleValue(answers[question.varName]).includes(value), true, option.label || value, () => toggleMultiple(question, value))}
                                    {:else}
                                        {@render choice(answers[question.varName] === value, false, option.label || value, () => selectSingle(question, value))}
                                    {/if}
                                {/each}
                            </div>
                        {/if}
                        {#if invalid}
                            <span class="text-xs text-draculared">{language.onboardingRequiredMissing}</span>
                        {/if}
                    </section>
                {/each}
            {/if}
        </div>

        <div class="flex items-center justify-end gap-2 px-5 py-3 border-t border-textcolor/10 shrink-0">
            <Button styled="outlined" size="sm" disabled={saving} onclick={skip}>{language.onboardingSkip}</Button>
            <Button size="sm" disabled={saving} onclick={finish} className={missingRequired ? 'opacity-60' : ''}>{language.onboardingDone}</Button>
        </div>
    </div>
</div>
