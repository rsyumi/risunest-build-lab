import { invoke } from "@tauri-apps/api/core";
import { SyncDriver, SyncStepError } from "../sync-ui/driver";
import { publishLibrary, receiveAndPush, reconnectAndPull } from "../sync-ui/roundTrip";
import { initialize } from "./contracts";

interface SyncInput {
  registration?: string;
  marker?: string;
  expect?: string;
}

const report = (stage: string, result: unknown) => invoke("macos_bench_report", { stage, result });

function need(value: string | undefined, name: string) {
  if (typeof value !== "string" || !value) throw new SyncStepError("sync-env", "input", `controller did not hand over ${name}`);
  return value;
}

/** One device of the round trip; the controller swaps profiles and hands each phase its inputs. */
export async function syncPhase(phase: string) {
  let input: SyncInput | null = null;
  try {
    input = JSON.parse((await invoke<string | null>("macos_bench_expected")) ?? "null");
  } catch {}
  if (!input) throw new SyncStepError("sync-env", "input", "controller did not hand over sync inputs");
  const driver = new SyncDriver(phase, report);
  const result = phase === "sync-publish"
    ? await publishLibrary(driver, { seed: initialize, registration: need(input.registration, "registration"), marker: need(input.marker, "marker"), characterId: "char-a" })
    : phase === "sync-receive"
      ? await receiveAndPush(driver, {
        seed: initialize, registration: need(input.registration, "registration"),
        expect: need(input.expect, "expected marker"), marker: need(input.marker, "marker"),
      })
      : phase === "sync-pull"
        ? await reconnectAndPull(driver, { expect: need(input.expect, "expected marker") })
        : undefined;
  if (!result) throw new SyncStepError("sync-env", "input", "unknown sync phase");
  await report(phase, { passed: true, label: "sync-result", ...result });
  await driver.quitWatchingExit(() => invoke("macos_bench_quit"));
}
