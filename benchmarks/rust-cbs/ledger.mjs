import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

// Coverage ledger for the native CBS engine. Code-derived fields are regenerated from the
// product sources; status and reason are reviewed classifications carried over by id.
export const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
export const ledgerPath = 'tests/fixtures/rust-core/cbs/ledger.json';
export const statuses = ['native', 'host', 'display', 'unsupported'];
// How the TextProcessor adapter (crates/cbs-experimental/src/cbs/processor.rs) treats a host or display entry
// when the caller hands it a host snapshot: `native` evaluates it and is replayed by the host fixture set,
// `decline` refuses it by name (adapterNote gives the name and what the product does), `pending` is not ported yet.
export const adapterStatuses = ['native', 'decline', 'pending'];
const adapted = status => status === 'host' || status === 'display';
const sources = {
  registrations: 'src/ts/cbs.ts',
  parser: 'src/ts/parser/parser.svelte.ts',
  monaco: 'src/ts/gui/codearea/cbsMonaco.ts',
};
const structuralFunctions = ['initMatcher', 'matcher', 'legacyBlockMatcher', 'blockStartMatcher', 'blockEndMatcher', 'risuChatParser'];

function parse(path) {
  const text = readFileSync(resolve(root, path), 'utf8');
  return { text, file: ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS) };
}

function visit(node, callback) {
  callback(node);
  ts.forEachChild(node, child => visit(child, callback));
}

function literal(node, path, what) {
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  throw new Error(`${path}: ${what} is not a string literal`);
}

function property(object, name) {
  return object.properties.find(p => ts.isPropertyAssignment(p) && p.name.getText() === name)?.initializer;
}

// matcher() looks callbacks up by toLocaleLowerCase() with whitespace, '_' and '-' removed;
// a key is reachable only when that normalization maps some name onto it.
const normalize = name => name.toLocaleLowerCase().replace(/[\s_-]/g, '');
// risuChatParser routes '#' and ':' to block handling before matcher() runs.
const structural = key => key.startsWith('#') || key.startsWith(':');

export function extractRegistrations() {
  const { file } = parse(sources.registrations);
  const calls = [];
  visit(file, node => {
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'registerFunction') calls.push(node);
  });
  const seen = new Map();
  const entries = calls.map(call => {
    const object = call.arguments[0];
    if (!object || !ts.isObjectLiteralExpression(object)) throw new Error('registerFunction argument is not an object literal');
    const name = literal(property(object, 'name'), sources.registrations, 'name');
    const aliasNode = property(object, 'alias');
    if (!aliasNode || !ts.isArrayLiteralExpression(aliasNode)) throw new Error(`${name}: alias is not an array literal`);
    const aliases = aliasNode.elements.map(e => literal(e, sources.registrations, `${name} alias`));
    const callbackNode = property(object, 'callback');
    if (!callbackNode) throw new Error(`${name}: missing callback`);
    const callback = ts.isStringLiteral(callbackNode) ? callbackNode.text : 'function';
    if (callback !== 'function' && callback !== 'doc_only') throw new Error(`${name}: unknown callback kind ${callback}`);
    const internal = property(object, 'internalOnly');
    const occurrence = (seen.get(name) ?? 0) + 1;
    seen.set(name, occurrence);
    return {
      id: occurrence === 1 ? name : `${name}@${occurrence}`,
      name, aliases, callback,
      internalOnly: internal?.kind === ts.SyntaxKind.TrueKeyword,
      deprecated: property(object, 'deprecated') !== undefined,
    };
  });
  // Runtime registration follows initMatcher(): doc_only is skipped and later keys overwrite earlier ones.
  const owner = new Map();
  entries.forEach((entry, index) => {
    if (entry.callback === 'doc_only') return;
    for (const key of [entry.name, ...entry.aliases]) owner.set(key, index);
  });
  for (const [index, entry] of entries.entries()) {
    const keys = entry.callback === 'doc_only' ? [] : [...new Set([entry.name, ...entry.aliases])];
    entry.runtimeKeys = keys;
    entry.shadowedKeys = keys.filter(key => owner.get(key) !== index);
    entry.reachableKeys = keys.filter(key => owner.get(key) === index && key !== '' && normalize(key) === key && !structural(key));
    entry.monaco = entry.internalOnly ? null : { name: entry.name.startsWith('#') ? entry.name.slice(1) : entry.name, isBlock: entry.name.startsWith('#') };
  }
  return entries;
}

// The registration implementations whose semantics extractRegistrations() mirrors.
const requiredSemantics = [
  [sources.parser, "if(callback === 'doc_only')"],
  [sources.parser, 'const names = [arg.name, ...arg.alias]'],
  [sources.parser, 'matcherMap.set(name, callback)'],
  [sources.parser, "if (/^[a-z0-9]+$/.test(name)) directMatcherMap.set(name, callback)"],
  [sources.parser, "directMatcherMap.get(name) ?? matcherMap.get(name.toLocaleLowerCase().replace(/[\\s_-]/g, ''))"],
  [sources.parser, "if(dat.startsWith('#') || dat.startsWith(':')){"],
  // Numeric constants the stream engine (crates/cbs-experimental/src/cbs/parser.rs) hardcodes.
  [sources.parser, 'let stackType = new Uint8Array(512)'],
  [sources.parser, 'arg.callStack = (arg.callStack ?? 0) + 1'],
  [sources.parser, 'if(arg.callStack > 20){'],
  [sources.parser, 'if(arg.callStack && arg.callStack > 20){'],
  [sources.parser, "const elseIndex = p1.indexOf('{{:else}}')"],
  [sources.parser, 'return p1.substring(elseIndex + 9)'],
  [sources.parser, 'const t2 = p1.substring(7).trim()'],
  [sources.parser, 'let t2 = p1.substring(5).trim()'],
  [sources.parser, 't2 = t2.substring(7).trim()'],
  [sources.parser, 't2 = t2.substring(3).trim()'],
  [sources.parser, 'let sub = blockType.type2.substring(asIndex + 4).trim()'],
  [sources.parser, "const statement = p1.split(' ', 2)"],
  [sources.monaco, 'if(arg.internalOnly){'],
  [sources.monaco, "const name = arg.name.startsWith('#') ? arg.name.slice(1) : arg.name;"],
];

export function checkSemantics() {
  const texts = Object.fromEntries(Object.values(sources).map(path => [path, readFileSync(resolve(root, path), 'utf8')]));
  return requiredSemantics.filter(([path, fragment]) => !texts[path].includes(fragment)).map(([path, fragment]) => `${path}: ${fragment}`);
}

export function extractStructural() {
  const { file } = parse(sources.parser);
  const forms = new Map();
  const add = (fn, kind, value) => {
    const id = `${fn}:${kind}:${value}`;
    if (!forms.has(id)) forms.set(id, { id, function: fn, kind, value });
  };
  const functions = new Map();
  visit(file, node => {
    if (ts.isFunctionDeclaration(node) && node.name && structuralFunctions.includes(node.name.text)) functions.set(node.name.text, node);
    if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && structuralFunctions.includes(node.name.text) && node.initializer) functions.set(node.name.text, node.initializer);
    if (ts.isTypeAliasDeclaration(node) && node.name.text === 'blockMatch' && ts.isUnionTypeNode(node.type)) {
      for (const member of node.type.types) add('blockMatch', 'type', member.literal.text);
    }
  });
  for (const fn of structuralFunctions) {
    const body = functions.get(fn);
    if (!body) throw new Error(`${sources.parser}: missing ${fn}`);
    visit(body, node => {
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) {
        const method = node.expression.name.text;
        const argument = node.arguments[0];
        if (['startsWith', 'endsWith'].includes(method) && argument && ts.isStringLiteral(argument)) add(fn, method, argument.text);
        if (method === 'includes' && ts.isArrayLiteralExpression(node.expression.expression)) {
          add(fn, 'includes', JSON.stringify(node.expression.expression.elements.map(e => literal(e, sources.parser, `${fn} includes`))));
        }
      }
      if (ts.isBinaryExpression(node) && [ts.SyntaxKind.EqualsEqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsEqualsToken].includes(node.operatorToken.kind)) {
        for (const side of [node.left, node.right]) if (ts.isStringLiteral(side)) add(fn, 'compare', side.text);
      }
      if (ts.isCaseClause(node) && ts.isStringLiteral(node.expression)) add(fn, 'case', node.expression.text);
      if (ts.isDefaultClause(node)) add(fn, 'default', node.parent.parent.expression.getText());
      if (ts.isRegularExpressionLiteral(node)) add(fn, 'regex', node.text);
    });
  }
  return [...forms.values()];
}

// Line endings depend on the checkout, so the digest covers the LF form.
const sha256 = path => createHash('sha256').update(readFileSync(resolve(root, path), 'utf8').replaceAll('\r\n', '\n')).digest('hex');

export function generate(previous = null) {
  const semantics = checkSemantics();
  if (semantics.length) throw new Error(`Registration semantics changed; update the ledger generator:\n${semantics.join('\n')}`);
  const carry = (list, id) => {
    const old = list?.find(entry => entry.id === id);
    const adapter = old && 'adapter' in old ? { adapter: old.adapter, ...('adapterNote' in old ? { adapterNote: old.adapterNote } : {}) } : {};
    return { status: old?.status ?? null, reason: old?.reason ?? null, ...adapter };
  };
  const registrations = extractRegistrations().map(entry => ({ ...entry, ...carry(previous?.registrations, entry.id) }));
  const structuralForms = extractStructural().map(entry => ({ ...entry, ...carry(previous?.structural, entry.id) }));
  const byStatus = list => Object.fromEntries(statuses.map(status => [status, list.filter(entry => entry.status === status).length]));
  const byAdapter = list => Object.fromEntries(adapterStatuses.map(status => [status, list.filter(entry => entry.adapter === status).length]));
  return {
    schema: 'risunest-cbs-ledger/1',
    generator: 'benchmarks/rust-cbs/ledger.mjs',
    sourceSha256: Object.fromEntries(Object.values(sources).map(path => [path, sha256(path)])),
    counts: {
      registrations: registrations.length,
      runtimeRegistrations: registrations.filter(entry => entry.callback === 'function').length,
      reachableKeys: registrations.reduce((n, entry) => n + entry.reachableKeys.length, 0),
      registrationStatus: byStatus(registrations),
      structural: structuralForms.length,
      structuralStatus: byStatus(structuralForms),
      adapterStatus: byAdapter([...registrations, ...structuralForms]),
    },
    registrations,
    structural: structuralForms,
  };
}

// Returns a list of problems; empty means the committed ledger matches the code.
export function check(committed) {
  const fresh = generate(committed);
  const problems = [];
  const { sourceSha256: _a, ...expected } = fresh;
  const { sourceSha256: _b, ...actual } = committed;
  if (JSON.stringify(expected) !== JSON.stringify(actual)) problems.push('ledger differs from the code; rerun with --write and classify new entries');
  for (const entry of [...fresh.registrations, ...fresh.structural]) {
    if (!statuses.includes(entry.status) || typeof entry.reason !== 'string' || entry.reason.trim() === '') problems.push(`unclassified: ${entry.id}`);
    if (adapted(entry.status)) {
      if (!adapterStatuses.includes(entry.adapter)) problems.push(`no adapter classification: ${entry.id}`);
      else if (entry.adapter !== 'native' && (typeof entry.adapterNote !== 'string' || entry.adapterNote.trim() === '')) problems.push(`no adapter note: ${entry.id}`);
    } else if ('adapter' in entry) problems.push(`adapter classification on a ${entry.status} entry: ${entry.id}`);
  }
  return problems;
}

export function readLedger() {
  return JSON.parse(readFileSync(resolve(root, ledgerPath), 'utf8'));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.includes('--write')) {
    let previous = null;
    try { previous = readLedger(); } catch { /* first generation */ }
    const ledger = generate(previous);
    writeFileSync(resolve(root, ledgerPath), JSON.stringify(ledger, null, 1) + '\n');
    console.log(JSON.stringify(ledger.counts));
  } else {
    const problems = check(readLedger());
    if (problems.length) { console.error(problems.join('\n')); process.exitCode = 1; }
    else console.log(JSON.stringify(readLedger().counts));
  }
}
