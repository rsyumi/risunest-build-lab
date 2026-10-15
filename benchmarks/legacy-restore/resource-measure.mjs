import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

const [binaryInput, ...manifestInputs] = process.argv.slice(2);
assert.ok(binaryInput && manifestInputs.length, 'Pass the compiled Rust test binary and synthetic fixture manifests');
const directory = path.resolve('.tmp', `native-restore-resources-${randomUUID()}`);
await mkdir(directory, { recursive: true });
const binary = path.join(directory, process.platform === 'win32' ? 'measure.exe' : 'measure');
await copyFile(path.resolve(binaryInput), binary);
const sha256 = async file => createHash('sha256').update(await readFile(file)).digest('hex');
const sourceFiles = [
  'src-tauri/src/local_backup.rs', 'src-tauri/src/native_file_jobs/legacy_backup.rs',
  'src-tauri/src/native_file_jobs/legacy_backup/account_encryption.rs',
  'src-tauri/src/native_file_jobs/restore.rs', 'src-tauri/src/native_file_jobs/restore/historical.rs',
  'src-tauri/src/native_file_jobs/restore/resource_measurements.rs',
  'src-tauri/src/asset_repository/import_payloads.rs', 'src-tauri/src/asset_repository/payload_cas.rs',
  'src-tauri/src/test_memory.rs', 'benchmarks/legacy-restore/resource-fixtures.mjs',
  'benchmarks/legacy-restore/fixture.ts',
];
const sourceHashes = Object.fromEntries(await Promise.all(sourceFiles.map(async file => [file, await sha256(file)])));
const report = { schema: 'risunest.synthetic-restore-resources-run/v1', platform: process.platform,
  binarySha256: await sha256(binary), sourceHashes, results: [], passed: false };
const reportPath = path.join(directory, 'result.json');
const save = () => writeFile(reportPath, JSON.stringify(report, null, 2));
await save();
for (const input of manifestInputs) {
  const manifestPath = path.resolve(input);
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  assert.equal(manifest.schema, 'risunest.synthetic-restore-resources/v1');
  assert.equal(manifest.synthetic, true);
  const resultPath = path.join(directory, `${report.results.length}.json`);
  const logPath = path.join(directory, `${report.results.length}.log`);
  const outcome = await new Promise((resolve, reject) => {
    const child = spawn(binary, ['native_file_jobs::restore::tests::native_backup_resource_measurement', '--ignored', '--exact', '--nocapture'], {
      windowsHide: true, env: { ...process.env, RISUNEST_RESTORE_RESOURCE_MANIFEST: manifestPath, RISUNEST_RESTORE_RESOURCE_REPORT: resultPath },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    child.stdout.on('data', bytes => { output += bytes; });
    child.stderr.on('data', bytes => { output += bytes; });
    child.on('error', reject);
    child.on('close', code => resolve({ code, output }));
  });
  await writeFile(logPath, outcome.output);
  assert.equal(outcome.code, 0, `Native resource case failed; see ${logPath}`);
  const result = JSON.parse(await readFile(resultPath, 'utf8'));
  assert.equal(result.verified, true);
  assert.equal(result.sourceSha256, manifest.sourceSha256);
  for (const file of sourceFiles.filter(file => file.startsWith('src-tauri/'))) {
    assert.equal(result.compiledSourceHashes?.[file], sourceHashes[file], `Compiled source differs: ${file}`);
  }
  assert.ok(Number.isSafeInteger(result.sampledPeakStagingBytes));
  assert.ok(Number.isSafeInteger(result.sampledPeakDatabaseBytes));
  report.results.push(result);
  await save();
  console.log(JSON.stringify({ axis: result.axis, size: result.size, peakWorkingSetBytes: result.peakWorkingSetBytes, sampledPeakOwnedDiskBytes: result.sampledPeakOwnedDiskBytes }));
}
for (const [file, expected] of Object.entries(sourceHashes)) assert.equal(await sha256(file), expected, `Measured source changed: ${file}`);
report.passed = true;
await save();
console.log(JSON.stringify({ passed: true, result: reportPath }));
