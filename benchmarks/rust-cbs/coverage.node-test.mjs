import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { check, readLedger } from './ledger.mjs';
import { caseText, fixtureSets, oracleSources, sourceDigests } from './fixtures/cases.mjs';
import { hostCases, hostClock, hostOracleSources, hostProfiles, hostSnapshots, hostViewport, hostZoneRange } from './fixtures/host-cases.mjs';

const fixture = set => JSON.parse(readFileSync(new URL(`./fixtures/${set}.json`, import.meta.url), 'utf8'));

test('the CBS ledger matches the product registrations', () => {
  assert.deepEqual(check(readLedger()), []);
});

test('committed fixtures hold the current case definitions with an expectation each', () => {
  for (const [set, { context, cases }] of Object.entries(fixtureSets)) {
    const committed = fixture(set);
    assert.deepEqual(committed.context, context, `${set} context`);
    const current = JSON.parse(JSON.stringify(cases));
    assert.deepEqual(committed.cases.map(item => item.id), current.map(item => item.id), `${set} case ids changed; run the generate mode`);
    committed.cases.forEach(({ expected, ...item }, i) => {
      assert.deepEqual(item, current[i], `${set}/${item.id} changed; run the generate mode`);
      assert.ok(expected && typeof expected === 'object', `${set}/${item.id} has no expectation`);
    });
  }
  const host = fixture('host');
  assert.deepEqual([host.clock, host.viewport, host.profiles, host.zoneRange, host.snapshots],
    JSON.parse(JSON.stringify([hostClock, hostViewport, hostProfiles, hostZoneRange, hostSnapshots])), 'host snapshots changed; run the generate mode');
  const current = JSON.parse(JSON.stringify(hostCases));
  assert.deepEqual(host.cases.map(({ expected, ...item }) => item), current, 'host cases changed; run the generate mode');
  for (const { id, steps, expected } of host.cases) {
    assert.ok(expected.steps.length >= 1 && expected.steps.length <= steps.length && Number.isInteger(expected.consumed), `host/${id} has no expectation`);
  }
});

// The adapter classification is reviewed: every family the adapter evaluates is replayed by a host case, and a
// host case names only such families.
test('host cases cover exactly the adapter-native ledger entries', () => {
  const ledger = readLedger();
  const native = [...ledger.registrations, ...ledger.structural].filter(entry => entry.adapter === 'native').map(entry => entry.id).sort();
  assert.deepEqual([...new Set(hostCases.flatMap(item => item.families))].sort(), native);
});

// A product edit can leave the fixtures behind; the run.mjs replay is the gate, so this only reports it.
test('committed fixtures record the product sources they were generated from', t => {
  for (const [set, sources] of [...Object.keys(fixtureSets).map(set => [set, oracleSources]), ['host', hostOracleSources]]) {
    const current = sourceDigests(sources);
    const recorded = fixture(set).sourceSha256 ?? {};
    const changed = sources.filter(path => recorded[path] !== current[path]);
    if (changed.length) {
      const notice = `${set} fixtures may be stale (${changed.join(', ')} changed since generation): rerun run.mjs generate, then run.mjs fixtures`;
      t.diagnostic(notice);
      console.warn(notice);
    }
  }
});

test('every reachable runtime key has a families case', () => {
  const texts = fixtureSets.families.cases.map(caseText);
  const invoked = key => texts.some(text => text.includes(`{{${key}}}`) || text.includes(`{{${key}::`));
  const missing = readLedger().registrations.flatMap(entry => entry.reachableKeys).filter(key => !invoked(key));
  assert.deepEqual(missing, []);
});
