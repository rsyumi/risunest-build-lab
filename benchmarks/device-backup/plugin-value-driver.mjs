import { writeFile } from 'node:fs/promises';
import path from 'node:path';

export async function waitForPluginMeasurement(evaluate) {
  const deadline = Date.now() + 120_000;
  while (!(await evaluate("typeof globalThis.__pluginValueMeasurement?.ready === 'function'"))) {
    if (Date.now() > deadline) throw new Error('Synthetic plugin harness did not initialize');
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  await evaluate('globalThis.__pluginValueMeasurement.ready()');
}

export async function measurePluginValue({ mebibytes, kind, directory, evaluate, memory, save }) {
  await waitForPluginMeasurement(evaluate);
  const result = { schema: 'risunest.synthetic-plugin-values/v1', synthetic: true, mebibytes, kind, stages: [], memory: [], phase: 'running' };
  for (const operation of ['write', 'read', 'backup', 'restore']) {
    const started = Date.now();
    await evaluate(`void globalThis.__pluginValueMeasurement.${operation}(${operation === 'write' ? `${mebibytes},${JSON.stringify(kind)}` : ''}); true`);
    let state;
    do {
      state = await evaluate('globalThis.__pluginValueMeasurement.state()');
      result.state = state;
      result.memory.push({ operation, phase: state.phase, ...await memory() });
      await save(result);
      if (state.phase === 'failed') throw new Error(`Synthetic plugin ${operation}: ${state.failure}`);
      if (Date.now() - started > 30 * 60_000) throw new Error(`Synthetic plugin ${operation} timed out`);
      if (state.busy) await new Promise(resolve => setTimeout(resolve, 500));
    } while (state.busy);
    if (state.phase !== 'verified') throw new Error(`Synthetic plugin ${operation} did not verify`);
    result.stages.push({ operation, elapsedMs: Date.now() - started, ...state });
  }
  result.phase = 'verified';
  await writeFile(path.join(directory, 'result.json'), JSON.stringify(result, null, 2));
  await save(result);
  return result;
}
