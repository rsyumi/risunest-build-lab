import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  SERVER_SYNC_DEVICE_CHANGED_EVENT,
  SERVER_SYNC_REMOTE_HINT_EVENT,
} from "./serverSyncNativeSignals";

const nativeSource = readFileSync(resolve("src-tauri/src/server_sync/events.rs"), "utf8");
const commandsSource = readFileSync(resolve("src-tauri/src/server_sync/commands.rs"), "utf8");

describe("native server sync signals", () => {
  it("names the same events the native side emits", () => {
    expect(nativeSource).toContain(`DEVICE_CHANGED_EVENT: &str = "${SERVER_SYNC_DEVICE_CHANGED_EVENT}"`);
    expect(commandsSource).toContain(`emit("${SERVER_SYNC_REMOTE_HINT_EVENT}"`);
  });
});
