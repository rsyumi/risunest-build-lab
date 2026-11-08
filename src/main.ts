// No normal app imports are allowed before the native device recovery gate.
// Native navigation and writer checks precede every normal app initialization.
import { deviceMaintenanceBeforeBootstrap } from "./ts/storage/deviceBackup/entry";
import { appCleanupBeforeBootstrap } from "./ts/storage/appCleanupEntry";

window.addEventListener("vite:preloadError", (event) => {
  console.error("Chunk load error detected:", event);
  alert(
    "The server has been updated or the network connection has been lost. Please refresh the page.",
  );
});

const app = appCleanupBeforeBootstrap().then(() => deviceMaintenanceBeforeBootstrap()).then(
  () => import("./normalMain"),
);
export default app;
