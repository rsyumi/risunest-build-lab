import { invoke } from "@tauri-apps/api/core";
import { getIdentifier } from "@tauri-apps/api/app";
import { platform } from "@tauri-apps/plugin-os";
import {
  LARGE_COMMIT_BYTES,
  NativeCommitTransport,
  encodeNativeCommit,
  type CommitEnvelope,
} from "../src/ts/storage/nativeCommitTransport";

const prefix = "synthetic-unicode-persistence-";
const nfd = "\u1112\u1161\u11ab\u1100\u1173\u11af-e\u0301-A\u030a";
const nestedKey = `key-${nfd}`;
const value = `${nfd}|\u1100\u1161|o\u0308|\u{1f43f}\ufe0f`;
const checkpointKey = `${prefix}checkpoint`;
const paddingKey = `${prefix}padding`;
const paddingLength = LARGE_COMMIT_BYTES * 2;
const routes = ["json", "raw"] as const;
type Route = (typeof routes)[number];
type Command = { command: string; body: "bytes" | "object"; serializedArgumentBytes: number };

function check(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(`Unicode persistence probe: ${message}`);
}
function points(text: string) {
  return Array.from(text, character => character.codePointAt(0)!);
}
async function guard() {
  const os = platform();
  check(["macos", "linux", "ios"].includes(os), "unsupported WebView platform");
  check(await getIdentifier() === `io.github.rsyumi.risunest.${os}.bench`, "isolated identifier required");
  check(nfd.normalize("NFC") !== nfd && value.normalize("NFC") !== value, "fixture is not decomposed");
  return os;
}
function observe(root: Record<string, unknown>, route: Route) {
  const expectedKey = `${prefix}${route}-${nfd}`;
  const actualKeys = Object.keys(root).filter(key => key.startsWith(`${prefix}${route}-`));
  const actualKey = actualKeys[0];
  const record = actualKey ? root[actualKey] : undefined;
  const object = record && typeof record === "object" && !Array.isArray(record)
    ? record as Record<string, unknown> : {};
  const actualNestedKeys = Object.keys(object);
  const actualNestedKey = actualNestedKeys[0];
  const actualValue = actualNestedKey ? object[actualNestedKey] : undefined;
  const keyEqual = actualKeys.length === 1 && actualKey === expectedKey;
  const nestedKeyEqual = actualNestedKeys.length === 1 && actualNestedKey === nestedKey;
  const valueEqual = actualValue === value;
  return {
    route, keyEqual, nestedKeyEqual, valueEqual,
    expectedKeyCodePoints: points(expectedKey), actualKeyCodePoints: actualKeys.map(points),
    expectedNestedKeyCodePoints: points(nestedKey), actualNestedKeyCodePoints: actualNestedKeys.map(points),
    expectedValueCodePoints: points(value), actualValueCodePoints: typeof actualValue === "string" ? points(actualValue) : null,
    exactCodePoints: keyEqual && nestedKeyEqual && valueEqual,
  };
}

export async function runUnicodePersistenceProbe(initialRevision: number) {
  const os = await guard();
  let revision = initialRevision;
  const commands: Command[] = [];
  const transport = new NativeCommitTransport({
    windows: () => false,
    macos: () => os === "macos",
    linux: () => os === "linux",
    ios: () => os === "ios",
    shared: () => undefined,
    encode: encodeNativeCommit,
    invoke: (command, args) => {
      commands.push({ command, body: args instanceof Uint8Array ? "bytes" : "object",
        serializedArgumentBytes: args instanceof Uint8Array ? args.byteLength : new TextEncoder().encode(JSON.stringify(args)).byteLength });
      return invoke(command, args);
    },
  });
  const cases: (ReturnType<typeof observe> & { transport: Command })[] = [];
  for (const route of routes) {
    commands.length = 0;
    const input: CommitEnvelope = {
      assetAliases: [],
      commit: {
        expectedRevision: revision,
        rootMutations: [{ type: "set", key: `${prefix}${route}-${nfd}`, value: { [nestedKey]: value } }],
      },
    };
    if (route === "raw") input.commit.rootMutations!.push({ type: "set", key: paddingKey, value: "x".repeat(paddingLength) });
    const committed = await transport.commit(input);
    check(committed.revision === revision + 1, "commit did not advance exactly one revision");
    revision = committed.revision;
    check(commands.length === 1 && commands[0].command === (route === "json" ? "pds_commit" : "pds_commit_raw")
      && commands[0].body === (route === "json" ? "object" : "bytes"), "unexpected transport selection");
    const read = await invoke<{ revision: number; value: Record<string, unknown> }>("pds_read_root");
    check(read.revision === revision, "read revision differs from commit");
    cases.push({ ...observe(read.value, route), transport: { ...commands[0] } });
  }
  const result = { schema: "synthetic-unicode-persistence-v1", platform: os, revision,
    rawPaddingBytes: paddingLength, cases, exactCodePoints: cases.every(item => item.exactCodePoints) };
  localStorage.setItem(checkpointKey, JSON.stringify({ platform: os, revision, transports: cases.map(item => item.transport) }));
  check(result.exactCodePoints, `immediate code points differ: ${JSON.stringify(result)}`);
  return result;
}

export async function verifyUnicodePersistenceProbe() {
  const os = await guard();
  const serialized = localStorage.getItem(checkpointKey);
  check(serialized, "reload requires the completed synthetic commit phase");
  const checkpoint = JSON.parse(serialized) as { platform: string; revision: number; transports: Command[] };
  check(checkpoint.platform === os && Number.isSafeInteger(checkpoint.revision), "reload checkpoint invalid");
  const read = await invoke<{ revision: number; value: Record<string, unknown> }>("pds_read_root");
  check(read.revision >= checkpoint.revision, "stored revision predates the Unicode commits");
  const cases = routes.map((route, index) => ({ ...observe(read.value, route), transport: checkpoint.transports[index] }));
  const padding = read.value[paddingKey];
  const paddingEqual = typeof padding === "string" && padding.length === paddingLength && /^x+$/.test(padding);
  const result = { schema: "synthetic-unicode-persistence-v1", platform: os, revision: read.revision,
    stage: "reload", paddingEqual, cases, exactCodePoints: cases.every(item => item.exactCodePoints) };
  check(result.exactCodePoints && paddingEqual, `reloaded code points differ: ${JSON.stringify(result)}`);
  return result;
}
