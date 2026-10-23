import { invoke } from "@tauri-apps/api/core";

// Isolated native-contract pages do not mount the product controller.
export async function showNativeHarness() {
  const policy = {
    enabled: false, timing: "launch", gateRequired: false,
    hideRecentsPreview: false, backgroundColor: "#1f2029",
  };
  const deadline = performance.now() + 15_000;
  do {
    const snapshot = await invoke<{ active: boolean; epoch: number }>("app_lock_set_policy", { policy });
    if (snapshot.active) {
      await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
      if (await invoke<boolean>("app_lock_ready", { epoch: snapshot.epoch, locked: false })) return;
    }
    await new Promise(resolve => setTimeout(resolve, 50));
  } while (performance.now() < deadline);
  throw new Error("Native harness cover handover timed out");
}
