import { openUrl } from "@tauri-apps/plugin-opener";
import { isTauri } from "./platform";
import { fetch as nativeFetch } from "@tauri-apps/plugin-http";
import { downloadFile } from "./globalApi.svelte";
import { alertError } from "./alert";
import { createWebviewDownloadHandler, installWebviewBridge, installMainFrameLinkBridge } from "./webviewDownload";

export function installNativeWebviewHandling() {
  if (!isTauri) return;
  const save = createWebviewDownloadHandler({ fetch: nativeFetch, save: (name, bytes) => downloadFile(name, bytes, { saveDialog: true }), reportError: error => alertError(String(error)) });
  const lifetime = new AbortController();
  window.addEventListener('pagehide', () => lifetime.abort(), { once: true });
  installWebviewBridge((name, source) => save(name, source, lifetime.signal), console.error, true);
  installMainFrameLinkBridge(openUrl, console.error);
}
