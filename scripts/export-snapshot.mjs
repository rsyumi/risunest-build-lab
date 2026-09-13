import { exportSnapshot, json } from "./snapshot.mjs";
const [repo, commit, lane, destination, ...extra] = process.argv.slice(2);
if (!destination || extra.length)
  throw new Error(
    "Usage: export-snapshot.mjs SOURCE_REPO FULL_COMMIT macos|ios EMPTY_DESTINATION",
  );
console.log(json(exportSnapshot(repo, commit, lane, destination)));
