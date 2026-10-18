import { openUrl } from "@tauri-apps/plugin-opener";
import { isTauri } from "./platform";
import { fetch as nativeFetch } from "@tauri-apps/plugin-http";
import { createWebviewDownloadHandler, installWebviewBridge, installMainFrameLinkBridge } from "./webviewDownload";

export function installNativeWebviewHandling() {
  if (!isTauri) return;
  const save = createWebviewDownloadHandler({
    fetch: nativeFetch,
    save: async (name, bytes) => {
      const { downloadFile } = await import("./globalApi.svelte");
      return downloadFile(name, bytes, { saveDialog: true });
    },
    reportError: async error => {
      const { alertError } = await import("./alert");
      alertError(String(error));
    },
  });
  const lifetime = new AbortController();
  window.addEventListener('pagehide', () => lifetime.abort(), { once: true });
  installWebviewBridge((name, source) => save(name, source, lifetime.signal), console.error, true);
  installMainFrameLinkBridge(openUrl, console.error);
}
