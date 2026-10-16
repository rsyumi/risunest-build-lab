import type { Plugin } from "vite";

export function viewportInstrumentation(): Plugin {
  return {
    name: "synthetic-viewport-parse-metrics",
    enforce: "pre",
    transform(code, id) {
      const file = id.replaceAll("\\", "/");
      if (file.endsWith("/src/lib/ChatScreens/DefaultChatScreen.svelte")) {
        const callback = "createBoundedContextSeed: (current) => ";
        if (!code.includes(callback)) throw new Error("Parser seed callback changed");
        return code.replace(callback, callback + "(globalThis as any).__parserSeedBaseline ? createBoundedLiveParserContext(current) : ");
      }
      if (file.endsWith("/src/ts/chatParserContextSeed.svelte.ts")) {
        for (const name of ["createChatParserContextSeed", "captureChatParserContextSeed", "forkChatParserContext"]) {
          const declaration = `export function ${name}(`;
          if (!code.includes(declaration)) throw new Error("Parser seed instrumentation boundary changed");
          code = code.replace(declaration, `function measured_${name}(`) + `
export function ${name}(...args: Parameters<typeof measured_${name}>) {
  const started = performance.now();
  try { return measured_${name}(...args); }
  finally { (globalThis as any).__parserSeedMetrics?.push({ name: '${name}', durationMs: performance.now() - started }); }
}
`;
        }
        return code;
      }
      if (!file.endsWith("/src/ts/parser/parser.svelte.ts")) return;
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
