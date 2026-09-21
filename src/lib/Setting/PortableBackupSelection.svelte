<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { language } from "../../lang";
  import type {
    DeviceSectionChoice,
    NativePortableDeviceSection,
  } from "../../ts/storage/deviceBackup/selection";
  import type {
    NativeArchiveInventory,
    NativePortableSelection,
  } from "../../ts/storage/nativeFileJobs";
  import type { DataHealthResult } from "../../ts/storage/dataHealth";
  import SettingButton from "./RisuNest/SettingButton.svelte";
  import SettingToggle from "./RisuNest/SettingToggle.svelte";

  /** One row treatment for every choice in this dialog. */
  const rowClass =
    "flex min-h-12 flex-wrap items-center gap-3 px-3 py-2 transition-colors hover:bg-selected";
  const boxedRowClass = `${rowClass} rounded-lg border border-darkborderc bg-darkbutton`;

  let {
    mode,
    choices,
    libraryIncluded,
    repairRequired = false,
    diagnosis,
    items,
    firstRun = false,
    onDone,
    onError,
  }: {
    mode: "export" | "restore";
    choices: DeviceSectionChoice<NativePortableDeviceSection>[];
    libraryIncluded: boolean;
    repairRequired?: boolean;
    /** What is wrong with the archive, so a refused one can still be looked at. */
    diagnosis?: DataHealthResult;
    /** The records an import can choose between; absent means the whole library only. */
    items?: NativeArchiveInventory;
    /** Nothing on the device is at stake, so the help describes an import, not an overwrite. */
    firstRun?: boolean;
    onDone: (selection: NativePortableSelection | null) => void;
    onError: (error: unknown) => void;
  } = $props();
  const text = language.portableBackup;
  let dialog: HTMLDialogElement;
  let finished = false;
  // A damaged archive cannot come in whole, but its undamaged records still can.
  let library = $state(untrack(() => libraryIncluded && !repairRequired));
  let partial = $state(untrack(() => repairRequired && Boolean(items)));
  let chosen = $state(
    untrack(() =>
      items
        ? {
            characters: items.characters
              .filter((entry) => entry.damaged === 0)
              .map((entry) => entry.id),
            presets: items.presets
              .filter((entry) => entry.damaged === 0)
              .map((entry) => entry.id),
            plugins: items.plugins
              .filter((entry) => entry.damaged === 0)
              .map((entry) => entry.id),
          }
        : { characters: [], presets: [], plugins: [] },
    ),
  );
  const groups = $derived(
    items
      ? ([
          ["characters", items.characters, text.itemCharacters],
          ["presets", items.presets, text.itemPresets],
          ["plugins", items.plugins, text.itemPlugins],
        ] as const)
      : [],
  );
  const chosenCount = $derived(
    chosen.characters.length + chosen.presets.length + chosen.plugins.length,
  );
  const damagedCount = $derived(
    (diagnosis?.counts.blocking ?? 0) + (diagnosis?.counts.degraded ?? 0),
  );
  function toggleItem(
    kind: "characters" | "presets" | "plugins",
    id: string,
  ): void {
    chosen = {
      ...chosen,
      [kind]: chosen[kind].includes(id)
        ? chosen[kind].filter((chosenId) => chosenId !== id)
        : [...chosen[kind], id],
    };
  }
  let sections = $state(
    untrack(() => choices.map((choice) => ({ ...choice }))),
  );
  const selectedCount = $derived(
    Number(library || (partial && chosenCount > 0)) +
      sections.filter((choice) => choice.selected).length,
  );
  const titleId = `portable-selection-${crypto.randomUUID()}`;

  function finish(selection: NativePortableSelection | null) {
    if (finished) return;
    finished = true;
    dialog.close();
    onDone(selection);
  }
  onMount(() => {
    try {
      dialog.showModal();
    } catch (error) {
      onError(error);
    }
    return () => {
      if (dialog.open) dialog.close();
    };
  });
</script>

<dialog
  bind:this={dialog}
  aria-labelledby={titleId}
  aria-describedby={`${titleId}-help`}
  class="m-auto w-[min(34rem,calc(100%-2rem))] max-h-[calc(100dvh-2rem)] overflow-hidden rounded-xl border border-darkborderc bg-bgcolor p-0 text-textcolor shadow-xl backdrop:bg-black/60"
  oncancel={(event) => {
    event.preventDefault();
    finish(null);
  }}
  onclose={() => finish(null)}
>
  <form
    class="flex max-h-[calc(100dvh-2rem)] flex-col"
    onsubmit={(event) => {
      event.preventDefault();
      if (!selectedCount) return;
      const sectionIds = sections
        .filter((choice) => choice.included && choice.selected)
        .map((choice) => choice.sectionId);
      if (partial && chosenCount > 0) {
        // Everything the archive holds but this selection does not name is excluded on purpose,
        // so closure never pulls a damaged record back in behind the reader.
        const named = new Set([
          ...chosen.characters,
          ...chosen.presets,
          ...chosen.plugins,
        ]);
        const excluded = [
          ...(items?.characters ?? []),
          ...(items?.presets ?? []),
          ...(items?.plugins ?? []),
        ]
          .map((entry) => entry.id)
          .filter((id) => !named.has(id));
        finish({
          library: true,
          deviceSections: sectionIds,
          items: { ...chosen, excluded },
        });
        return;
      }
      finish({
        library: libraryIncluded && !repairRequired && library,
        deviceSections: sectionIds,
      });
    }}
  >
    <header class="border-b border-darkborderc px-5 pt-5 pb-4">
      <p class="mb-1 text-xs font-medium tracking-wide text-textcolor2">
        {text.title} · .risunest
      </p>
      <h2 id={titleId} class="text-xl font-semibold">
        {mode === "export" ? text.chooseExport : text.chooseRestore}
      </h2>
      <p
        id={`${titleId}-help`}
        class="mt-2 text-sm leading-relaxed text-textcolor2"
      >
        {mode === "export"
          ? text.helpExport
          : firstRun
            ? text.helpRestoreFirstRun
            : text.helpRestore}
      </p>
    </header>
    <div class="overflow-y-auto px-5 py-4">
      {#if repairRequired}
        <p
          class="mb-4 rounded-lg border border-darkborderc bg-darkbutton p-3 text-sm"
          role="note"
        >
          {text.repair}
        </p>
      {/if}
      {#if damagedCount > 0}
        <p
          data-portable-diagnosis
          class="mb-4 rounded-lg border border-darkborderc bg-darkbutton p-3 text-sm"
          role="note"
        >
          {text.damaged.replace("{0}", damagedCount.toLocaleString())}
        </p>
      {/if}
      {#if items && groups.some((group) => group[1].length > 0)}
        <div class="{boxedRowClass} mb-4">
          <SettingToggle label={text.choosePart} showLabel bind:checked={partial} />
        </div>
        {#if partial}
          {#each groups as [kind, entries, label] (kind)}
            {#if entries.length > 0}
              <fieldset data-portable-items={kind} class="mb-4 min-w-0">
                <legend
                  class="mb-2 text-xs font-medium tracking-wide text-textcolor2"
                >
                  {label}
                </legend>
                <div
                  class="divide-y divide-darkborderc rounded-lg border border-darkborderc"
                >
                  {#each entries as entry (entry.id)}
                    <div class={rowClass}>
                      <SettingToggle
                        label={entry.id}
                        showLabel
                        checked={chosen[kind].includes(entry.id)}
                        onchange={() => toggleItem(kind, entry.id)}
                      />
                      {#if entry.damaged > 0}
                        <span class="ml-auto shrink-0 text-xs text-textcolor2"
                          >{text.itemDamaged.replace(
                            "{0}",
                            entry.damaged.toLocaleString(),
                          )}</span
                        >
                      {/if}
                    </div>
                  {/each}
                </div>
              </fieldset>
            {/if}
          {/each}
          <p class="mb-4 text-sm text-textcolor2">{text.choosePartHelp}</p>
        {/if}
      {/if}
      {#if libraryIncluded}
        <div class="{boxedRowClass} mb-4">
          <SettingToggle
            label={text.library}
            showLabel
            disabled={repairRequired}
            bind:checked={library}
          />
        </div>
      {/if}
      {#if sections.some((choice) => choice.sectionId !== "local-settings")}
        <fieldset class="min-w-0">
          <legend
            class="mb-2 text-xs font-medium tracking-wide text-textcolor2"
          >
            {text.device}
          </legend>
          <div
            class="divide-y divide-darkborderc rounded-lg border border-darkborderc"
          >
            {#each sections.filter((choice) => choice.sectionId !== "local-settings") as choice (choice.sectionId)}
              <div class={rowClass}>
                <SettingToggle
                  label={choice.label}
                  showLabel
                  bind:checked={choice.selected}
                />
              </div>
            {/each}
          </div>
        </fieldset>
      {/if}
      {#each sections.filter((choice) => choice.sectionId === "local-settings") as choice (choice.sectionId)}
        <div class="{rowClass} mt-4 rounded-lg border border-darkborderc">
          <SettingToggle
            label={text.settings}
            showLabel
            bind:checked={choice.selected}
          />
        </div>
      {/each}
    </div>
    <footer
      class="flex shrink-0 justify-end gap-2 border-t border-darkborderc bg-darkbg px-5 py-4"
    >
      <SettingButton variant="secondary" onclick={() => finish(null)}>
        {text.cancel}
      </SettingButton>
      <SettingButton type="submit" disabled={!selectedCount}>
        {text.continue}
      </SettingButton>
    </footer>
  </form>
</dialog>
