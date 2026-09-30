import type { Plugin } from "vite";

export function viewportInstrumentation(): Plugin {
  return {
    name: "synthetic-viewport-parse-metrics",
    enforce: "pre",
    transform(code, id) {
      if (!id.replaceAll("\\", "/").endsWith("/src/ts/parser/parser.svelte.ts")) return;
      const declaration = "export async function ParseMarkdown(";
      if (!code.includes(declaration)) throw new Error("ParseMarkdown instrumentation boundary changed");
      return code.replace(declaration, "async function measuredParseMarkdown(") + `
export async function ParseMarkdown(...args: Parameters<typeof measuredParseMarkdown>) {
  const metrics = (globalThis as any).__viewportParseMetrics;
  const started = performance.now();
  if (metrics) metrics.active++;
  try { return await measuredParseMarkdown(...args); }
  finally {
    if (metrics) {
      metrics.active--;
      metrics.calls.push({ index: args[3] ?? -1, durationMs: performance.now() - started });
    }
  }
}
`;
    },
  };
}
