import { readFileSync } from "node:fs";
import { defineConfig, type Plugin } from "vite";
import { harnessConfig } from "../harnessConfig";

const config = harnessConfig("macos");
export default defineConfig(async environment => {
  const resolved = await config(environment);
  if (process.env.RISUNEST_APPEARANCE_PROBE === "1") {
    const entry = (resolved.plugins as Plugin[]).find(plugin => plugin.name === "harness-html-entry");
    if (!entry) throw new Error("Harness HTML transform missing");
    entry.transformIndexHtml = {
      order: "pre",
      handler() {
        const html = readFileSync(new URL("../../index.html", import.meta.url), "utf8");
        if (!html.includes('src="/src/main.ts"')) throw new Error("Product HTML entry changed");
        return html.replace('src="/src/main.ts"', 'src="/benchmarks/macos/main.ts"');
      },
    };
  }
  return resolved;
});
