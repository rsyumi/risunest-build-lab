import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it, vi } from "vitest";
import {
  SERVER_SYNC_DEVICE_CHANGED_EVENT,
  SERVER_SYNC_REMOTE_HINT_EVENT,
  subscribeNativeServerSyncSignals,
} from "./serverSyncNativeSignals";

const nativeSource = readFileSync(
  resolve("src-tauri/src/server_sync/events.rs"),
  "utf8",
);
const commandsSource = readFileSync(resolve("src-tauri/src/server_sync/commands.rs"), "utf8");
const notificationSource = readFileSync(resolve("src-tauri/src/server_sync/notification.rs"), "utf8");
const wireSource = readFileSync(resolve("crates/sync-wire/src/lww.rs"), "utf8");
const productionSource = readFileSync(resolve("src/ts/storage/sync/serverSyncProduction.ts"), "utf8");

describe("native server sync signals", () => {
  it("wakes synchronization when a device revision changes or the remote may have moved", async () => {
    const handlers = new Map<string, () => void>();
    const dispose = vi.fn();
    const deviceChanged = vi.fn();
    const remoteHint = vi.fn();
    const stop = subscribeNativeServerSyncSignals(
      { deviceChanged, remoteHint },
      async (event, handler) => {
        handlers.set(event, handler);
        return dispose;
      },
    );
    await Promise.resolve();
    handlers.get(SERVER_SYNC_DEVICE_CHANGED_EVENT)?.();
    handlers.get(SERVER_SYNC_REMOTE_HINT_EVENT)?.();
    expect(deviceChanged).toHaveBeenCalledTimes(1);
    expect(remoteHint).toHaveBeenCalledTimes(1);
    expect(deviceChanged).toHaveBeenCalledWith();
    expect(remoteHint).toHaveBeenCalledWith();
    stop();
    expect(dispose).toHaveBeenCalledTimes(2);
  });
  it("names the same events the native side emits", () => {
    expect(nativeSource).toContain(
      `DEVICE_CHANGED_EVENT: &str = "${SERVER_SYNC_DEVICE_CHANGED_EVENT}"`,
    );
    expect(commandsSource).toContain(`events.emit("${SERVER_SYNC_REMOTE_HINT_EVENT}",frame)`);
    expect(productionSource).toContain(`listen('${SERVER_SYNC_REMOTE_HINT_EVENT}', () => scheduler.remoteHint())`);
  });
  it("emits an empty device change and a validated sequence hint", () => {
    expect(nativeSource).toContain("app.emit(DEVICE_CHANGED_EVENT, ())");
    expect(notificationSource).toContain("serde_json::from_str::<SeqNotification>(&body)");
    expect(notificationSource).toContain("notice(frame)");
    expect(wireSource).toMatch(/pub enum SeqNotification\s*\{\s*Seq \{ seq: DecimalU64 \},\s*\}/);
    expect(commandsSource).toContain(`events.emit("${SERVER_SYNC_REMOTE_HINT_EVENT}",frame)`);
  });
  it("checks binding authority and cleanup admission before starting notifications", () => {
    const start = commandsSource.slice(commandsSource.indexOf("pub(crate) async fn server_sync_notify_start("));
    const launch = start.indexOf("super::notification::run(");
    expect(launch).toBeGreaterThan(0);
    const admission = start.slice(0, launch);
    expect(admission).toContain("server_sync_notify_stop(app.clone()).await?");
    expect(admission).toContain("store.lww_binding_authority()?!=request.binding_authority");
    expect(admission.match(/cleanup_closed\.load\(Ordering::Acquire\)/g)).toHaveLength(1);
    expect(admission).toContain("install_notification(start,");
    const installStart = commandsSource.indexOf("fn install_notification(");
    const install = commandsSource.slice(installStart, commandsSource.indexOf("*slot=Some(spawn(", installStart));
    expect(install).toContain("self.cleanup_closed.load(Ordering::Acquire)");
    expect(install).toContain("start.cancelled.load(Ordering::Acquire)");
    const stopStart = commandsSource.indexOf("pub(crate) async fn server_sync_notify_stop(");
    const stop = commandsSource.slice(stopStart, commandsSource.indexOf("pub(crate) async fn server_sync_notify_start("));
    expect(stop).toContain("job.abort();let _=job.await;");
  });
});
