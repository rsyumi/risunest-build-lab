import { invoke } from "@tauri-apps/api/core";

// Isolated native-contract pages do not mount the product controller.
export async function showNativeHarness() {
  const policy = {
    enabled: false, timing: "launch", gateRequired: false,
    hideRecentsPreview: false, backgroundColor: "#1f2029",
  };
  const deadline = performance.now() + 15_000;
  let lastSnapshot: { active: boolean; epoch: number } | null = null;
  let lastReady: boolean | null = null;
  let policyAttempts = 0;
  let readyAttempts = 0;
  do {
    const snapshot = await invoke<{ active: boolean; epoch: number }>("app_lock_set_policy", { policy });
    lastSnapshot = snapshot;
    policyAttempts++;
    if (snapshot.active) {
      await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
      readyAttempts++;
      lastReady = await invoke<boolean>("app_lock_ready", { epoch: snapshot.epoch, locked: false });
      if (lastReady) return;
    }
    await new Promise(resolve => setTimeout(resolve, 50));
  } while (performance.now() < deadline);
  throw new Error(`Native harness cover handover timed out: ${JSON.stringify({
    lastSnapshot, lastReady, policyAttempts, readyAttempts,
    visibility: document.visibilityState, focused: document.hasFocus(),
  })}`);
}
