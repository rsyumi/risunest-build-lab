import "../../src/ts/polyfill";
import "core-js/actual";
import "katex/dist/katex.min.css";
import { isTauri } from "../../src/ts/platform";
import { getDeviceSettings } from "../../src/ts/storage/deviceSettings";
import { initializeDeviceMarkers } from "../../src/ts/storage/deviceMarkers";
import { startStreamingSmoke } from "./fixture";

if (isTauri) await initializeDeviceMarkers();
getDeviceSettings();
startStreamingSmoke();
