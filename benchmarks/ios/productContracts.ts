import { invoke } from "@tauri-apps/api/core";
import { check, initialize, pause } from "./contracts";

const expectedKey = "ios-synthetic-product-edit";
async function acceptTerms() {
  const { createNativeDeviceSettings } = await import(
    "../../src/ts/storage/nativeDeviceSettings"
  );
  await createNativeDeviceSettings().set("risunest_tos_v1", "true");
}

async function until(predicate: () => boolean, message: string) {
  const deadline = performance.now() + 60_000;
  while (!predicate()) {
    check(performance.now() < deadline, message);
    await pause(100);
  }
}

function storedProfile(stored: string | null): string | null {
  try {
    return stored === null ? null : JSON.parse(stored).performanceProfile ?? null;
  } catch {
    return "unreadable";
  }
}

/** The performance profile buttons of the RisuNest settings page, opened as a reader opens it. */
async function profileButtons() {
  const { language } = await import("../../src/lang");
  const { SettingsMenuIndex, settingsOpen } = await import("../../src/ts/stores.svelte");
  SettingsMenuIndex.set(17);
  settingsOpen.set(true);
  const find = (label: string) => [...document.querySelectorAll<HTMLButtonElement>("button")]
    .find((button) => button.textContent?.trim() === label);
  const lowSpec = () => find(language.risuNest.perf.profileLowSpec);
  await until(() => Boolean(lowSpec()), "RisuNest performance profile did not render");
  return { lowSpec, close: () => settingsOpen.set(false) };
}

/** Product UI and storage, mounted only by the isolated verification entry. */
export async function productApp(restart: boolean) {
  if (restart) {
    await invoke("pds_open");
    const stored = localStorage.getItem(expectedKey);
    const expected = JSON.parse(stored!);
    check(expected, "previous synthetic product edit required", { stored });
    const saved = await invoke<{ revision: number; value: { message: { data: string }[] } }>(
      "pds_read_conversation",
      { characterId: "char-a", conversationId: expected.conversationId },
    );
    check(
      saved.value.message.at(-1)?.data === expected.marker,
      "product edit survives process restart",
      {
        stored,
        actual: {
          revision: saved.revision,
          messages: saved.value.message.length,
          last: saved.value.message.at(-1)?.data,
        },
      },
    );
    const storedSetting = await invoke<string | null>("pds_get_device_setting", { key: "risuNestDeviceSettings" });
    document.getElementById("benchmark")!.remove();
    const app = await import("../../src/main");
    await app.default;
    await until(
      () => performance.getEntriesByName("boot:interactive").length > 0,
      "product Svelte app did not initialize after the restart",
    );
    const { getDeviceSettings, updateDeviceSettings, flushDeviceSettings } = await import(
      "../../src/ts/storage/deviceSettings"
    );
    const buttons = await profileButtons();
    const profile = {
      stored: storedProfile(storedSetting),
      loaded: getDeviceSettings().performanceProfile,
      shown: buttons.lowSpec()?.getAttribute("aria-pressed") ?? null,
    };
    buttons.close();
    check(
      profile.stored === "low-spec" && profile.loaded === "low-spec" && profile.shown === "true",
      "low-spec performance profile survives process restart",
      profile,
    );
    // Later phases start from the default profile again.
    updateDeviceSettings({ performanceProfile: "normal" });
    await flushDeviceSettings();
    return { passed: true, restarted: true, profile };
  }
  await initialize();
  const opened = await invoke<{ revision: number }>("pds_open");
  await invoke("pds_commit", {
    commit: {
      expectedRevision: opened.revision,
      rootMutations: [{ type: "set", key: "didFirstSetup", value: true }],
    },
    assetAliases: [],
  });
  await acceptTerms();
  document.getElementById("benchmark")!.remove();
  const app = await import("../../src/main");
  await app.default;
  const { getPersistentDataRuntime } = await import(
    "../../src/ts/storage/persistentDataRuntime.svelte"
  );
  await until(() => {
    try {
      return (
        Boolean(getPersistentDataRuntime().store) &&
        performance.getEntriesByName("boot:interactive").length > 0 &&
        document.getElementById("app")!.textContent!.length > 100
      );
    } catch {
      return false;
    }
  }, "product Svelte app did not initialize");
  const { DBState } = await import("../../src/ts/stores.svelte");
  const { changeChar } = await import("../../src/ts/characters");
  const { tick } = await import("svelte");
  const index = DBState.db.characters.findIndex(
    (character) => character.chaId === "char-a",
  );
  check(
    index >= 0 && (await changeChar(index)),
    "open synthetic character in product UI",
  );
  const marker = "ios-synthetic-ui-edit 🐿️";
  const runtime = getPersistentDataRuntime();
  const lease = await runtime.acquireCompleteConversation("edit-message");
  let conversationId: string;
  try {
    const { captureChatMessageTarget, saveCapturedChatMessage } = await import(
      "../../src/ts/chatMessageUi"
    );
    const context = {
      captureCurrent: () => {
        const character = DBState.db.characters[index];
        return { character, conversation: character.chats[character.chatPage] };
      },
      getCurrentSession: () => runtime.getActiveConversationSession(),
    };
    conversationId = lease.session.conversationId;
    const target = captureChatMessageTarget({
      ...context,
      absoluteIndex: lease.session.totalMessages - 1,
    });
    check(target, "capture product message edit target");
    check(
      saveCapturedChatMessage(target, context, marker).saved,
      "product message edit accepted",
    );
  } finally {
    lease.release();
  }
  await tick();
  await runtime.flushPendingData("ios-ui-smoke");
  await until(
    () => document.getElementById("app")!.textContent!.includes(marker),
    "edited synthetic message did not render in product chat",
  );
  const persisted = await invoke<{
    revision: number;
    value: { message: { data: string }[] };
  }>("pds_read_conversation", { characterId: "char-a", conversationId });
  check(
    persisted.value.message.at(-1)?.data === marker,
    "product edit persisted through Rust",
    { revision: persisted.revision, last: persisted.value.message.at(-1)?.data },
  );
  localStorage.setItem(expectedKey, JSON.stringify({ conversationId, marker }));
  const buttons = await profileButtons();
  buttons.lowSpec()!.click();
  await until(
    () => buttons.lowSpec()?.getAttribute("aria-pressed") === "true",
    "low-spec performance profile was not selected",
  );
  // A reader leaves without waiting for anything, so only a short pause separates the tap from the process end.
  await pause(2_000);
  const selectedProfile = storedProfile(
    await invoke<string | null>("pds_get_device_setting", { key: "risuNestDeviceSettings" }),
  );
  buttons.close();
  return {
    passed: true,
    selectedProfile,
    revision: persisted.revision,
    renderedTextLength: document.getElementById("app")!.textContent!.length,
  };
}

/** Product onboarding, mounted with a reset synthetic working set. */
export async function productOnboarding() {
  await initialize();
  const opened = await invoke<{ revision: number }>("pds_open");
  await invoke("pds_commit", {
    commit: {
      expectedRevision: opened.revision,
      rootMutations: [
        { type: "set", key: "didFirstSetup", value: false },
        { type: "set", key: "language", value: "en" },
      ],
    },
    assetAliases: [],
  });
  await acceptTerms();
  document.getElementById("benchmark")!.remove();
  const app = await import("../../src/main");
  await app.default;
  await until(
    () => document.getElementById("app")!.textContent!.includes("Choose how to start."),
    "product onboarding did not initialize",
  );
  return { passed: true };
}

/** RisuNest settings, mounted directly for physical-device visual checks. */
export async function productSettings() {
  await initialize();
  const opened = await invoke<{ revision: number }>("pds_open");
  await invoke("pds_commit", {
    commit: {
      expectedRevision: opened.revision,
      rootMutations: [
        { type: "set", key: "didFirstSetup", value: true },
        { type: "set", key: "language", value: "en" },
      ],
    },
    assetAliases: [],
  });
  await acceptTerms();
  document.getElementById("benchmark")!.remove();
  const app = await import("../../src/main");
  await app.default;
  const { SettingsMenuIndex, settingsOpen } = await import(
    "../../src/ts/stores.svelte"
  );
  SettingsMenuIndex.set(17);
  settingsOpen.set(true);
  await until(
    () => document.getElementById("app")!.textContent!.includes("Performance"),
    "RisuNest settings did not initialize",
  );
  return { passed: true };
}
