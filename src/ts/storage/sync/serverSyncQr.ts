import { isTauriAndroid, isTauriIOS } from "src/ts/platform";
export const canScanServerRegistration = isTauriAndroid || isTauriIOS;
export { openAppSettings as openServerRegistrationSettings } from "@tauri-apps/plugin-barcode-scanner";
import {
  createQrScanner,
  type QrScanApi,
  type QrScanTone,
  type QrScanView,
} from "src/ts/ui/qrScanner";
import { parseServerRegistration } from "./serverSyncRegistration";
import type { ServerConfig } from "./serverSync";
export function createServerQrScanner(api?: QrScanApi, view?: QrScanView) {
  const scanner = createQrScanner(api, view);
  return {
    async scan(tone?: QrScanTone): Promise<ServerConfig> {
      return parseServerRegistration(await scanner.scan(tone));
    },
    cancel: scanner.cancel,
  };
}
