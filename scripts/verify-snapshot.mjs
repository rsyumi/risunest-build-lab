import { verifySnapshot, json } from "./snapshot.mjs";
const [root, lane, ...extra] = process.argv.slice(2);
if (!lane || extra.length)
  throw new Error("Usage: verify-snapshot.mjs SNAPSHOT_ROOT macos|ios");
console.log(json(verifySnapshot(root, lane)));
