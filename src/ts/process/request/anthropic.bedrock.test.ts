import { createHash, createHmac } from "node:crypto";
import { beforeEach, expect, it, vi } from "vitest";
import { LLMFormat } from "src/ts/model/types";
import { requestClaude } from "./anthropic";

const mocks = vi.hoisted(() => ({
  db: {
    applyAdditionalParamsToAll: true,
    additionalParams: [] as [string, string][],
    customModels: [],
  },
}));
vi.mock("src/ts/globalApi.svelte", () => ({
  addFetchLog: vi.fn(),
  fetchNative: vi.fn(),
  globalFetch: vi.fn(),
  textifyReadableStream: vi.fn(),
}));
vi.mock("src/ts/model/modellist", async () => ({
  ...(await import("src/ts/model/types")),
}));
vi.mock("src/ts/observer.svelte", () => ({ registerClaudeObserver: vi.fn() }));
vi.mock("src/ts/storage/database.svelte", () => ({
  getDatabase: () => mocks.db,
}));
vi.mock("src/ts/util", () => ({
  replaceAsync: async (text: string) => text,
  simplifySchema: vi.fn(),
  sleep: vi.fn(),
}));
vi.mock("../templates/jsonSchema", () => ({ extractJSON: vi.fn() }));
vi.mock("../mcp/mcp", () => ({
  callTool: vi.fn(),
  decodeToolCall: vi.fn(),
  encodeToolCall: vi.fn(),
}));

const host = "bedrock-runtime.us-east-1.amazonaws.com";
const path = "/model/us.anthropic.synthetic-model-v1/invoke";
const secret = "synthetic-secret";

beforeEach(() => {
  vi.spyOn(console, "log").mockImplementation(() => undefined);
});

const sha256 = (value: string) =>
  createHash("sha256").update(value, "utf8").digest("hex");
const hmac = (key: string | Buffer, value: string) =>
  createHmac("sha256", key).update(value, "utf8").digest();

// AWS SigV4 trims and collapses ASCII spaces only.
const canonicalValue = (value: string) =>
  value.replace(/^ +| +$/g, "").replace(/ {2,}/g, " ");

it.each([
  ["collapses ASCII spaces", "  synthetic   value "],
  ["preserves a no-break space", " synthetic value "],
])("signs Bedrock headers per SigV4 and %s", async (_name, value) => {
  mocks.db.additionalParams = [["header::x-synthetic", value]];
  const response = await requestClaude({
    formated: [{ role: "user", content: "synthetic" }],
    bias: {},
    aiModel: "synthetic-bedrock",
    key: `AKIDSYNTHETIC:${secret}:us-east-1`,
    maxTokens: 16,
    mode: "model",
    previewBody: true,
    useStreaming: false,
    modelInfo: {
      id: "synthetic-bedrock",
      format: LLMFormat.AWSBedrockClaude,
      flags: [],
      parameters: [],
      internalID: "anthropic.synthetic-model-v1",
    },
  } as any);
  expect(response.type).toBe("success");
  const preview = JSON.parse(response.result as string);
  expect(preview.url).toBe(`https://${host}${path}`);
  const headers = preview.headers as Record<string, string>;
  const date = headers["x-amz-date"];
  expect(date).toMatch(/^\d{8}T\d{6}Z$/);
  const payloadHash = sha256(JSON.stringify(preview.body));
  expect(headers["x-amz-content-sha256"]).toBe(payloadHash);
  expect(headers["x-synthetic"]).toBe(value);

  const signedHeaders =
    "accept;content-type;host;x-amz-content-sha256;x-amz-date;x-synthetic";
  const canonicalRequest = [
    "POST",
    path,
    "",
    "accept:application/json",
    "content-type:application/json",
    `host:${host}`,
    `x-amz-content-sha256:${payloadHash}`,
    `x-amz-date:${date}`,
    `x-synthetic:${canonicalValue(value)}`,
    "",
    signedHeaders,
    payloadHash,
  ].join("\n");
  const scope = `${date.slice(0, 8)}/us-east-1/bedrock/aws4_request`;
  const stringToSign = [
    "AWS4-HMAC-SHA256",
    date,
    scope,
    sha256(canonicalRequest),
  ].join("\n");
  let key = hmac(`AWS4${secret}`, date.slice(0, 8));
  for (const part of ["us-east-1", "bedrock", "aws4_request"])
    key = hmac(key, part);
  const signature = createHmac("sha256", key)
    .update(stringToSign, "utf8")
    .digest("hex");

  expect(headers.authorization).toBe(
    `AWS4-HMAC-SHA256 Credential=AKIDSYNTHETIC/${scope}, ` +
      `SignedHeaders=${signedHeaders}, Signature=${signature}`,
  );
});
