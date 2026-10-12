import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const bash = process.env.RISUNEST_TEST_BASH ?? "bash";
const skip = process.platform === "win32" && !process.env.RISUNEST_TEST_BASH;
const script = fileURLToPath(new URL("../../scripts/retry-command.sh", import.meta.url)).replaceAll("\\", "/");

function run(command, args = []) {
  return spawnSync(bash, ["-c", `
    sleep() { printf 'wait:%s\\n' "$1"; }
    fixture() { ${command}; }
    export -f sleep fixture
    exec bash "$@"
  `, "test", script, "fixture", ...args], { encoding: "utf8", timeout: 10000, windowsHide: true });
}

test("command retries preserve literal arguments and stop on success", { skip }, () => {
  const result = run('fixture_calls=$((${fixture_calls:-0} + 1)); printf "attempt:%s arg:%s\\n" "$fixture_calls" "$1"; [ "$fixture_calls" -ge 2 ]',
    ["literal spaces; $(exit 99)"]);
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split(/\r?\n/), [
    "attempt:1 arg:literal spaces; $(exit 99)", "wait:10", "attempt:2 arg:literal spaces; $(exit 99)",
  ]);
});

test("command retries return the final failure and do not retry interrupted commands", { skip }, () => {
  const failure = run("printf 'attempt\\n'; return 7");
  assert.ifError(failure.error);
  assert.equal(failure.status, 7);
  assert.deepEqual(failure.stdout.trim().split(/\r?\n/), ["attempt", "wait:10", "attempt", "wait:20", "attempt"]);
  const interrupted = run("printf 'attempt\\n'; return 130");
  assert.equal(interrupted.status, 130);
  assert.equal(interrupted.stdout.trim(), "attempt");
});
