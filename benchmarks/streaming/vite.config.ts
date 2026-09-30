import { defineConfig, mergeConfig } from "vite";
import { harnessConfig } from "../harnessConfig";
import { viewportInstrumentation } from "./viewportInstrumentation";

const base = harnessConfig("streaming");
export default defineConfig(async (environment) => mergeConfig(
  typeof base === "function" ? await base(environment) : base,
  { plugins: [viewportInstrumentation()] },
));
