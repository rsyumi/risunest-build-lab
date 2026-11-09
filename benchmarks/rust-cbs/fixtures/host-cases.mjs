// Host-snapshot cases for the native TextProcessor adapter. Every case evaluates its steps with the real
// risuChatParser against a synthetic database built from a named snapshot, Date fixed at `hostClock` and
// Math.random replaying the case tape; `run.mjs generate` records the output and the setChatVar calls of
// each step and the tape values drawn. `families` names the ledger ids a case exercises.
import { fixtureClock, oracleSources } from './cases.mjs';

export const hostClock = fixtureClock;
// window.innerWidth and innerHeight of the oracle page.
export const hostViewport = { width: 1280, height: 720 };
// The time zone and default locale of each oracle page; a case runs on `utc` unless it names another.
export const hostProfiles = {
  utc: { timeZone: 'UTC', locale: 'ko-KR' },
  newYork: { timeZone: 'America/New_York', locale: 'en-US' },
  lordHowe: { timeZone: 'Australia/Lord_Howe', locale: 'en-GB' },
  cairo: { timeZone: 'Africa/Cairo', locale: 'ar-EG' },
};
// The instants the captured zone covers, the sampling step that finds its transitions, and the instants whose
// transitions the local date probes surround.
export const hostZoneRange = {
  start: Date.UTC(1900, 0, 1), end: Date.UTC(2100, 0, 1), step: 3600000, probeStart: Date.UTC(2026, 0, 1), probeEnd: Date.UTC(2027, 0, 1),
};

// The owner's capture of the runtime's local time, evaluated in the page: the offsets Date's local getters apply
// over the range, as [start, offset] transitions at least one step apart, and the default locale's
// toLocaleTimeString() and toLocaleDateString() as parts filled from the local fields, or null when the output
// depends on more than those fields (another calendar or numbering system, hour or day period text that changes
// within an hour, parts that change with the value). `localDates` pairs local date-times around the probed
// transitions, and the first of each month, with the instant `new Date(y, m, d, h, min)` gives each.
export function captureLocalTime({ start, end, step, probeStart, probeEnd }) {
  const minutes = t => new Date(t).getTimezoneOffset();
  const offset = t => {
    const d = new Date(t);
    return Date.UTC(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours(), d.getMinutes(), d.getSeconds(), d.getMilliseconds()) - t;
  };
  const transitions = [[start, offset(start)]];
  for (let t = start; t < end;) {
    const next = Math.min(t + step, end);
    if (minutes(next) !== minutes(t)) {
      let lo = t, hi = next;
      while (hi - lo > 1) {
        const mid = Math.floor((lo + hi) / 2);
        if (minutes(mid) === minutes(lo)) lo = mid; else hi = mid;
      }
      if (hi < end) transitions.push([hi, offset(hi)]);
      t = hi;
    } else t = next;
  }
  const localDates = [];
  const probe = local => {
    const d = new Date(local);
    localDates.push([local, new Date(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), d.getUTCHours(), d.getUTCMinutes()).getTime()]);
  };
  for (const [at] of transitions.slice(1)) {
    if (at < probeStart || at >= probeEnd) continue;
    for (const m of [-90, -30, 0, 15, 29, 30, 45, 59, 60, 90]) probe(at + offset(at - 1) + m * 60000);
  }
  for (let m = 0; m < 12; m++) probe(Date.UTC(2026, m, 1));
  const plain = format => {
    const o = format.resolvedOptions();
    return o.calendar === 'gregory' && o.numberingSystem === 'latn';
  };
  const digits = (value, n) => (value === String(n) ? false : value === String(n).padStart(2, '0') ? true : null);
  const timeFormat = (() => {
    const format = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: 'numeric', second: 'numeric' });
    if (!plain(format)) return null;
    let layout = null, twoDigit = null;
    const hours = [], dayPeriods = [];
    for (let h = 0; h < 24; h++) {
      for (const [m, sec] of [[0, 0], [4, 5], [30, 30], [59, 59]]) {
        const date = new Date(2001, 1, 3, h, m, sec);
        if (date.getHours() !== h || date.getMinutes() !== m) return null;
        const parts = format.formatToParts(date);
        if (parts.map(p => p.value).join('') !== date.toLocaleTimeString()) return null;
        if (parts.some(p => !['literal', 'hour', 'minute', 'second', 'dayPeriod'].includes(p.type))) return null;
        const shape = JSON.stringify(parts.map(p => (p.type === 'literal' ? { literal: p.value } : { field: p.type })));
        if (layout === null) layout = shape; else if (shape !== layout) return null;
        const value = type => parts.find(p => p.type === type)?.value ?? '';
        const widths = [digits(value('minute'), m), digits(value('second'), sec)];
        if (widths.includes(null) || (m === 0 && widths[0] !== widths[1])) return null;
        if (twoDigit === null && m === 4) twoDigit = widths;
        if (m === 4 && (widths[0] !== twoDigit[0] || widths[1] !== twoDigit[1])) return null;
        if (m === 0) { hours[h] = value('hour'); dayPeriods[h] = value('dayPeriod'); }
        else if (hours[h] !== value('hour') || dayPeriods[h] !== value('dayPeriod')) return null;
      }
    }
    const parts = JSON.parse(layout).map(p => (p.field === 'minute' ? { field: 'minute', twoDigit: twoDigit[0] } : p.field === 'second' ? { field: 'second', twoDigit: twoDigit[1] } : p));
    return { parts, hours, dayPeriods };
  })();
  const dateFormat = (() => {
    const format = new Intl.DateTimeFormat(undefined, { year: 'numeric', month: 'numeric', day: 'numeric' });
    if (!plain(format)) return null;
    let layout = null, twoDigit = null;
    for (const [y, m, d] of [[2001, 1, 3], [1999, 10, 23], [2026, 11, 31]]) {
      const date = new Date(y, m, d, 12);
      if (date.getFullYear() !== y || date.getMonth() !== m || date.getDate() !== d) return null;
      const parts = format.formatToParts(date);
      if (parts.map(p => p.value).join('') !== date.toLocaleDateString()) return null;
      if (parts.some(p => !['literal', 'year', 'month', 'day'].includes(p.type))) return null;
      const shape = JSON.stringify(parts.map(p => (p.type === 'literal' ? { literal: p.value } : { field: p.type })));
      if (layout === null) layout = shape; else if (shape !== layout) return null;
      const value = type => parts.find(p => p.type === type)?.value ?? '';
      if (value('year') !== String(y)) return null;
      const widths = [digits(value('month'), m + 1), digits(value('day'), d)];
      if (widths.includes(null)) return null;
      // The first probe's single-digit month and day fix the widths; two-digit values cannot tell them apart.
      if (twoDigit === null) twoDigit = widths;
    }
    const parts = JSON.parse(layout).map(p => (p.field === 'month' ? { field: 'month', twoDigit: twoDigit[0] } : p.field === 'day' ? { field: 'day', twoDigit: twoDigit[1] } : p));
    return { parts };
  })();
  return { zone: { transitions, end }, timeFormat, dateFormat, localDates };
}

// Snapshot descriptions: `database` is the Database fragment the page normalizes, `selected` the
// selectedCharID and `triggerId` the CurrentTriggerIdStore value.
const message = (role, data, extra = {}) => ({ role, data, ...extra });
const aster = (chat = {}, overrides = {}) => ({
  type: 'character', chaId: 'syn-aster', name: 'Aster', nickname: '', chatPage: 0,
  personality: 'Kind to {{user}}', desc: 'Calm {{getvar::mood}}', scenario: 'A quiet library', exampleMessage: 'Example by {{char}}',
  firstMessage: 'First {{user}}', alternateGreetings: ['Alt A', 'Alt {{char}} B'],
  defaultVariables: 'mood=calm\nlevel=1\nempty=fallback\n=nokey\nnovalue=\nmulti=a=b\nlevel=2',
  chats: [{
    id: 'syn-chat-1', name: 'Synthetic', fmIndex: -1, note: 'Note for {{char}}', localLore: [],
    scriptstate: { $mood: 'happy', $count: 3, $nullish: null, $empty: '' },
    message: [
      message('char', 'Welcome, {{user}}.', { time: 1767225000000, chatId: 'c0', saying: '', generationInfo: { model: 'synthetic-model', inputTokens: 12 } }),
      message('user', 'Hi {{char}}::wave', { time: 1767225060000, chatId: 'c1' }),
      message('char', 'Fine {{setvar::seen::1}}{{getvar::mood}}', { time: 1767225120000, chatId: 'c2' }),
      message('user', 'Plain "quoted" \\ text\nline', { chatId: 'c3' }),
      message('char', 'Last <user> line', { time: 1767225240000, chatId: 'c4' }),
    ],
    ...chat,
  }],
  emotionImages: [], additionalAssets: [], globalLore: [], customscript: [], triggerscript: [], bias: [], sdData: [], tags: [],
  ...overrides,
});
const database = (characters, overrides = {}) => ({
  characters, username: 'Mina', personaPrompt: 'Mina likes {{char}}', mainPrompt: 'Main for {{char}}', jailbreak: 'JB {{user}}',
  globalNote: 'Global {{getvar::mood}}', templateDefaultVariables: 'theme=dark\nlevel=9\nmood=tmpl',
  globalChatVariables: { shared: 'g1', toggle_demo: '1', toggle_off: '0', toggle_word: 'yes', localonly: 'db', blank: 'db-blank' },
  promptTemplate: [{ type: 'plain', type2: 'normal', text: 'x', role: 'system' }, { type: 'authornote', defaultText: '' }, { type: 'authornote', defaultText: 'Template note {{user}}' }],
  aiModel: 'synthetic-main', subModel: 'synthetic-sub', jailbreakToggle: true, maxContext: 8192,
  modules: [], plugins: [], botPresets: [], regex: [], personas: [],
  ...overrides,
});
const lore = (key, content, extra = {}) => ({ key, secondkey: '', insertorder: 100, comment: key, content, mode: 'normal', alwaysActive: false, selective: false, ...extra });
const customModel = (id, name, format, flags, extra = {}) => ({ id, name, url: '', format, tokenizer: 0, key: '', params: '', flags, ...extra });
// Message times around the 2026 transitions of the profile zones, an invalid one, a missing one and a string.
const timed = [
  message('user', 'u0', { time: 1767225000000 }),
  message('char', 'c1', { time: 1772953199999 }),
  message('user', 'u2', { time: 1772953200000 }),
  message('char', 'c3', { time: 1793512800000 }),
  message('user', 'u4', { time: 1775314800000 }),
  message('user', 'u5', { time: 1767225060000 }),
  message('char', 'c6', { time: 1e300 }),
  message('user', 'u7'),
  message('char', 'c8', { time: '2026-01-01' }),
  message('user', 'u9', { time: 1791041400000 }),
];
export const hostSnapshots = {
  full: { database: database([aster({ GLGlobalVariables: { localonly: 'chat', blank: '' } })]), selected: 0, triggerId: 'trig-7' },
  nickname: {
    database: database([aster({ fmIndex: 1, note: '' }, { nickname: 'Ast' })], { jailbreakToggle: false, maxContext: 4096 }),
    selected: 0, triggerId: null,
  },
  greetingMissing: { database: database([aster({ fmIndex: 5 })]), selected: 0, triggerId: null },
  emptyChat: { database: database([aster({ message: [], id: undefined })]), selected: 0, triggerId: null },
  noTemplate: { database: database([aster({ note: '' })], { promptTemplate: [{ type: 'authornote' }] }), selected: 0, triggerId: null },
  noCharacter: { database: database([]), selected: 0, triggerId: null },
  plain: {
    database: database([aster({}, { desc: 'd', personality: 'p{{personality}}', scenario: '{{comment::hidden}}S' })]),
    selected: 0, triggerId: null,
  },
  functions: {
    database: database([aster({}, { personality: '{{#func f}}[{{arg::1}}]{{/func}}{{call::f::z}}', scenario: '{{call::f::y}}' })]),
    selected: 0, triggerId: null,
  },
  nested: {
    database: database([aster({}, { desc: 'D{{setvar::fromdesc::1}}{{getvar::fromdesc}}', personality: '{{random::a::b}}' })]),
    selected: 0, triggerId: null,
  },
  library: {
    database: database([aster({ localLore: [lore('chat-key', 'Chat lore {{char}}')] }, {
      globalLore: [lore('aster', 'Aster lore'), lore('quote "x"::y', 'Line\nbreak', { alwaysActive: true })],
      emotionImages: [['happy', 'assets/happy.png'], ['sad::x', 'assets/sad.png']],
      additionalAssets: [['map', 'assets/map.png', 'png'], ['hidden', 'assets/hidden.png', 'png'], ['note', 'assets/note.txt', 'txt']],
      prebuiltAssetCommand: true, prebuiltAssetExclude: ['assets/hidden.png', 'map'],
    })], {
      modules: [
        { id: 'mod-a', name: 'Module A', description: '', namespace: 'ns.a', lorebook: [lore('module-a', 'Module lore')], assets: [['icon', 'assets/icon.png', 'png'], ['banner::wide', 'assets/banner.png', 'png']] },
        { id: 'mod-b', name: 'Module B', description: '', lorebook: [lore('module-b', 'B lore')] },
        { id: 'mod-c', name: 'Module C', description: '', namespace: 'ns.c', assets: [['off', 'assets/off.png', 'png']] },
      ],
      enabledModules: ['mod-a', 'mod-b'], language: 'ko',
      aiModel: 'xcustom:::syn-anth', customModels: [customModel('xcustom:::syn-anth', 'Syn Anth', 2, [4], { internalId: 'syn-anth-int' })],
    }),
    selected: 0, triggerId: null,
  },
  library2: {
    database: database([aster({}, { emotionImages: undefined, additionalAssets: undefined, prebuiltAssetCommand: true })]),
    selected: 0, triggerId: null,
  },
  claude: { database: database([aster()], { aiModel: 'claude-synthetic' }), selected: 0, triggerId: null },
  legacy: {
    database: database([aster()], { aiModel: 'xcustom:::syn-legacy', customModels: [customModel('xcustom:::syn-legacy', 'Syn Legacy', 3, [4])] }),
    selected: 0, triggerId: null,
  },
  anthropicPlain: {
    database: database([aster()], { aiModel: 'xcustom:::syn-plain', customModels: [customModel('xcustom:::syn-plain', 'Syn Plain', 2, [], { internalId: '' })] }),
    selected: 0, triggerId: null,
  },
  // `windowStart` attaches a history window to the selected chat: its loaded messages start at that absolute index.
  windowed: { database: database([aster()]), selected: 0, triggerId: null, windowStart: 10 },
  windowedZero: { database: database([aster()]), selected: 0, triggerId: null, windowStart: 0 },
  clocked: { database: database([aster({ message: timed })]), selected: 0, triggerId: 'trig-c' },
  lastUntimed: { database: database([aster({ message: [message('user', 'x', { time: 5 }), message('char', 'y')] })]), selected: 0, triggerId: null },
};

// A step is one risuChatParser call; `apply` writes [key, value or null] through setChatVar before it.
// `projectedChatID`, `historyOffset`, `currentTime` and `triggerId` are the arg fields of that name, and `history`
// builds its historyAccess: { length, rows: [[index, row]], complete: [rows], searches: [[role, before, count, rows]] }.
// A step with `refusal` is the last of its case, and the adapter declines it with that reason. A case runs on the
// page of `profile` (default `utc`) with Date fixed at `clock` (default `hostClock`).
const cases = [];
const add = (id, families, snapshot, steps, extra = {}) =>
  cases.push({ id, families, snapshot, steps: steps.map(s => (typeof s === 'string' ? { text: s } : s)), tape: [], ...extra });

// Character and persona.
add('char-name', ['char'], 'full', ['{{char}}|{{bot}}|{{ Char }}']);
add('char-nickname', ['char'], 'nickname', ['{{char}}|{{bot}}']);
add('char-none', ['char', 'personality'], 'noCharacter', ['{{char}}|{{personality}}']);
add('char-consistant', ['char', 'user'], 'full', ['{{char}}|{{user}}'], { consistantChar: true });
add('char-tokenize', ['char', 'personality'], 'noCharacter', ['{{char}}|{{bot}}|{{personality}}'], { tokenizeAccurate: true });
add('user', ['user'], 'full', ['{{user}}|{{USER}}']);
add('fields', ['personality', 'description', 'scenario', 'exampledialogue'], 'full',
  ['{{personality}}/{{charpersona}}/{{description}}/{{chardesc}}/{{scenario}}/{{exampledialogue}}/{{examplemessage}}/{{example_dialogue}}']);
add('persona', ['persona'], 'full', ['{{persona}}|{{userpersona}}']);
add('prompts', ['mainprompt', 'jb', 'globalnote'], 'full',
  ['{{mainprompt}}|{{systemprompt}}|{{main_prompt}}|{{jb}}|{{jailbreak}}|{{globalnote}}|{{systemnote}}|{{ujb}}']);
add('authornote-chat', ['authornote'], 'full', ['{{authornote}}|{{author_note}}']);
add('authornote-template', ['authornote'], 'nickname', ['{{authornote}}']);
add('authornote-none', ['authornote'], 'noTemplate', ['[{{authornote}}]']);
add('authornote-no-character', ['authornote'], 'noCharacter', ['{{authornote}}']);
add('trigger-id', ['trigger_id'], 'full', ['{{trigger_id}}|{{triggerid}}']);
add('trigger-id-null', ['trigger_id'], 'nickname', ['{{triggerid}}']);
add('settings', ['model', 'axmodel', 'jbtoggled', 'maxcontext'], 'full', ['{{model}}|{{axmodel}}|{{jbtoggled}}|{{maxcontext}}']);
add('settings-off', ['jbtoggled', 'maxcontext'], 'nickname', ['{{jbtoggled}}|{{maxcontext}}']);
add('nested-call-stack', ['description'], 'plain', ['{{description}}'.repeat(21)]);
add('nested-recursion', ['personality'], 'plain', ['{{personality}}']);
add('nested-display', ['scenario'], 'plain', [{ text: '{{comment::shown}}{{scenario}}', visualize: true }, '{{comment::x}}{{scenario}}']);
add('nested-functions', ['personality', 'scenario'], 'functions', ['{{personality}}{{personality}}{{scenario}}', '{{scenario}}']);
add('nested-effects', ['description'], 'nested', [{ text: '{{description}}|{{getvar::fromdesc}}', runVar: true }, '{{description}}']);
add('nested-random', ['personality', 'random'], 'nested', ['{{personality}}{{personality}}'], { tape: [0.75, 0.25] });

// Explicit parser callbacks are a per-call variable scope; the following unscoped step still reads the database.
add('scope-writers-conditions-reparse', ['setvar', 'addvar', 'setdefaultvar', 'getvar', 'description', 'trigger_id'], 'full', [
  { text: '{{setvar::mood::local}}{{addvar::level::2}}{{setdefaultvar::new::x}}{{#when::mood::vis::local}}Y{{:else}}N{{/when}}|{{getvar::level}}|{{description}}|{{#func f}}{{getvar::new}}/{{triggerid}}{{/func}}{{call::f}}',
    runVar: true, scope: { variables: { mood: 'bound', level: '40' }, triggerId: 'inner' } },
  '{{getvar::mood}}|{{triggerid}}',
  { text: '{{triggerid}}|{{#when::var::mood}}Y{{:else}}N{{/when}}', scope: { variables: {}, triggerId: null } },
]);
add('scope-arguments-and-utf16', ['setvar', 'getvar'], 'full', [
  { text: '{{setvar}}{{setvar::}}{{setvar::x}}{{getvar::s}}', runVar: true, scope: { variables: { s: '\ud800' } } },
  { text: '{{setvar::x::value}}', scope: { variables: {} } },
  { text: '{{setvar::x::value}}', runVar: true, rmVar: true, scope: { variables: {} } },
]);
add('scope-nested-writes', ['description', 'setvar', 'getvar'], 'nested', [
  { text: '{{description}}|{{getvar::fromdesc}}', runVar: true, scope: { variables: { mood: 'scoped' } } },
  '{{getvar::fromdesc}}',
]);
add('applied-message-and-note-view', ['history', 'previouschatlog', 'lastmessage', 'lastmessageid', 'authornote'], 'full', [
  { text: '{{history}}', edits: [{ kind: 'setData', index: -5.8, data: 'edited {{getvar::mood}}' }] },
  { text: '{{lastmessageid}}|{{lastmessage}}|{{history::role}}', edits: [{ kind: 'append', role: 'user', data: 'third', chatId: 'new-id' }] },
  { text: '{{lastmessageid}}|{{previouschatlog::0}}|{{history}}', edits: [{ kind: 'cut', start: 1.5, end: -1 }] },
  { text: '{{authornote}}', edits: [{ kind: 'note', data: 'N{{getvar::mood}}' }] },
  { text: '{{authornote}}', edits: [{ kind: 'note', data: '' }] },
]);

// History.
for (const [index, label] of [[3, 'mid'], [-1, 'end'], [0, 'start'], [9, 'past'], [-5, 'negative']])
  add(`previouscharchat-${label}`, ['previouscharchat'], 'full', [{ text: '{{previouscharchat}}|{{lastcharmessage}}', chatIndex: index }]);
add('previouscharchat-alternate', ['previouscharchat'], 'nickname', [{ text: '{{previouscharchat}}', chatIndex: 0 }]);
add('previouscharchat-missing-greeting', ['previouscharchat'], 'greetingMissing', [{ text: '{{previouscharchat}}', chatIndex: 0 }]);
add('previouscharchat-empty', ['previouscharchat'], 'emptyChat', ['{{previouscharchat}}']);
add('previouscharchat-no-character', ['previouscharchat'], 'noCharacter', ['{{previouscharchat}}']);
for (const [index, label] of [[-1, 'none'], [4, 'mid'], [1, 'greeting'], [0, 'start'], [9, 'past']])
  add(`previoususerchat-${label}`, ['previoususerchat'], 'full', [{ text: '[{{previoususerchat}}|{{lastusermessage}}]', chatIndex: index }]);
add('previoususerchat-no-character', ['previoususerchat'], 'noCharacter', [{ text: '{{previoususerchat}}', chatIndex: 0 }]);
add('histories', ['userhistory', 'charhistory'], 'full', ['{{userhistory}}', '{{charhistory}}', '{{usermessages}}{{user_history}}{{charmessages}}{{char_history}}']);
add('histories-run-var', ['charhistory'], 'full', [{ text: '{{charhistory}}{{getvar::seen}}', runVar: true }]);
add('histories-empty', ['userhistory', 'charhistory'], 'emptyChat', ['{{userhistory}}|{{charhistory}}']);
add('histories-no-character', ['userhistory', 'charhistory'], 'noCharacter', ['{{userhistory}}|{{charhistory}}']);
add('history', ['history'], 'full', ['{{history}}', '{{history::role}}', '{{messages::role::x}}', '{{history::}}', '{{history::data}}']);
add('history-run-var', ['history'], 'full', [{ text: '{{history}}', runVar: true, rmVar: true }]);
add('history-alternate', ['history'], 'nickname', ['{{history}}']);
add('history-empty', ['history'], 'emptyChat', ['{{history}}|{{history::role}}']);
add('history-missing-greeting', ['history'], 'greetingMissing', ['{{history}}|{{history::role}}']);
add('history-no-character', ['history'], 'noCharacter', ['{{history}}|{{history::role}}']);
add('last-message', ['lastmessage', 'lastmessageid'], 'full', ['{{lastmessage}}|{{lastmessageid}}|{{lastmessageindex}}']);
add('last-message-empty', ['lastmessage', 'lastmessageid'], 'emptyChat', ['{{lastmessage}}|{{lastmessageid}}']);
add('last-message-no-character', ['lastmessage', 'lastmessageid'], 'noCharacter', ['[{{lastmessage}}|{{lastmessageid}}]']);
add('previouschatlog', ['previouschatlog'], 'full', [
  '{{previouschatlog::0}}|{{previouschatlog::4}}|{{previouschatlog::5}}|{{previouschatlog::-1}}|{{previouschatlog::1.0}}',
  '{{previouschatlog::-0}}|{{previouschatlog::abc}}|{{previouschatlog}}|{{previouschatlog::0x1}}|{{previouschatlog:: 2 }}|{{previous_chat_log::1e0}}',
  '{{previouschatlog::1.5}}|{{previouschatlog::}}|{{previouschatlog::Infinity}}',
]);
add('previouschatlog-no-character', ['previouschatlog'], 'noCharacter', ['{{previouschatlog::0}}']);
add('firstmsgindex', ['firstmsgindex'], 'full', ['{{firstmsgindex}}|{{firstmessageindex}}|{{first_msg_index}}']);
add('firstmsgindex-alternate', ['firstmsgindex'], 'nickname', ['{{firstmsgindex}}']);
add('firstmsgindex-no-character', ['firstmsgindex'], 'noCharacter', ['{{firstmsgindex}}']);
add('timestamps', ['messageunixtimearray'], 'full', ['{{messageunixtimearray}}|{{message_unixtime_array}}']);
add('timestamps-empty', ['messageunixtimearray'], 'emptyChat', ['{{messageunixtimearray}}']);
add('timestamps-no-character', ['messageunixtimearray'], 'noCharacter', ['{{messageunixtimearray}}']);
add('chatindex', ['chatindex'], 'full', [{ text: '{{chatindex}}|{{chat_index}}', chatIndex: 3 }, '{{chatindex}}']);
add('role', ['role'], 'full', [
  { text: '{{role}}', chatRole: 'system', firstmsg: true }, { text: '{{role}}', firstmsg: true, chatIndex: 1 }, { text: '{{role}}', chatIndex: 1 },
  { text: '{{role}}', chatIndex: 0 }, { text: '{{role}}', chatIndex: 9 }, { text: '{{role}}', role: 'user' }, '{{role}}',
]);
add('role-no-character', ['role'], 'noCharacter', [{ text: '{{role}}', chatIndex: 0 }, { text: '{{role}}', role: 'char' }]);
add('isfirstmsg', ['isfirstmsg'], 'full', [{ text: '{{isfirstmsg}}|{{isfirstmessage}}', firstmsg: true }, '{{isfirstmsg}}']);

// Variables.
add('getvar', ['getvar'], 'full', [
  '{{getvar::mood}}|{{getvar::count}}|{{getvar::nullish}}|[{{getvar::empty}}]|{{getvar::level}}|{{getvar::theme}}|{{getvar::multi}}',
  '{{getvar::novalue}}|{{getvar::missing}}|{{getvar}}|{{getvar::}}|{{getvar::toString}}|{{getvar::__proto__}}|{{getvar::constructor}}',
]);
add('getvar-no-character', ['getvar'], 'noCharacter', ['{{getvar::mood}}|{{getvar::theme}}']);
for (const runVar of [false, true]) for (const rmVar of [false, true])
  add(`write-flags-${runVar}-${rmVar}`, ['setvar', 'addvar', 'setdefaultvar'], 'full',
    [{ text: '{{setvar::mood::sad}}|{{addvar::count::2}}|{{setdefaultvar::fresh::new}}|{{getvar::mood}}{{getvar::count}}{{getvar::fresh}}', runVar, rmVar }]);
add('write-sequence', ['setvar', 'addvar', 'setdefaultvar'], 'full', [
  { text: '{{setvar::mood::sad}}{{getvar::mood}}|{{setvar::level}}{{getvar::level}}|{{setdefaultvar::level::d}}{{getvar::level}}', runVar: true },
  { text: '{{setvar::x}}{{setdefaultvar::x::d}}{{getvar::x}}|{{setvar::x::1}}{{setvar::x::1}}{{getvar::x}}', runVar: true },
  { text: '{{addvar::count::2}}{{getvar::count}}|{{addvar::mood::1}}|{{addvar::newcount::1}}|{{addvar::theme::0}}|{{addvar::level::1.5}}{{getvar::level}}', runVar: true },
  { text: '{{setdefaultvar::empty::filled}}{{getvar::empty}}|{{setdefaultvar::nullish::n}}{{getvar::nullish}}|{{setdefaultvar::count::9}}{{getvar::count}}', runVar: true },
  { text: '{{setvar}}{{getvar::undefined}}|{{setvar::::v}}{{getvar::}}|{{setvar::{{getvar::mood}}::k}}{{getvar::sad}}', runVar: true },
  { text: '{{setvar::toString::t}}{{getvar::toString}}|{{getvar::mood}}{{getvar::count}}{{getvar::x}}' },
]);
add('write-no-character', ['setvar', 'addvar', 'setdefaultvar'], 'noCharacter',
  [{ text: '{{setvar::a::1}}|{{addvar::b::1}}|{{setdefaultvar::c::1}}|{{getvar::a}}', runVar: true }]);
add('write-apply', ['getvar', 'setvar'], 'full', [
  { text: '{{getvar::mood}}', apply: [['mood', 'applied']] },
  { text: '{{getvar::mood}}|{{getvar::level}}', apply: [['mood', null], ['level', '7']] },
  { text: '{{setvar::a::1}}', runVar: true },
  '{{getvar::a}}',
]);
add('getglobalvar', ['getglobalvar'], 'full',
  ['{{getglobalvar::shared}}|{{getglobalvar::localonly}}|{{getglobalvar::blank}}|{{getglobalvar::missing}}|{{getglobalvar::toggle_demo}}|{{getglobalvar}}']);
add('getglobalvar-no-character', ['getglobalvar'], 'noCharacter', ['{{getglobalvar::localonly}}|{{getglobalvar::blank}}']);
add('getglobalvar-prototype', ['getglobalvar'], 'full', [{ text: '{{getglobalvar::toString}}', refusal: 'variable-prototype-key' }]);
add('when-variables', ['blockStartMatcher:case:var', 'blockStartMatcher:case:toggle', 'blockStartMatcher:case:vis', 'blockStartMatcher:case:visnot',
  'blockStartMatcher:case:tis', 'blockStartMatcher:case:tisnot'], 'full', [
  '{{#when::var::mood}}V{{/when}}|{{#when::var::missing}}V{{:else}}v{{/when}}|{{#when::var::level}}L{{/when}}|{{#when::var::empty}}E{{:else}}e{{/when}}',
  '{{#when::toggle::demo}}T{{/when}}|{{#when::toggle::off}}T{{:else}}t{{/when}}|{{#when::toggle::localonly}}G{{:else}}g{{/when}}',
  '{{#when::mood::vis::happy}}A{{/when}}|{{#when::level::vis::1}}B{{/when}}|{{#when::mood::visnot::sad}}C{{/when}}|{{#when::theme::visnot::dark}}D{{:else}}d{{/when}}',
  '{{#when::word::tis::yes}}E{{/when}}|{{#when::word::tisnot::no}}F{{/when}}|{{#when::demo::tis::0}}G{{:else}}g{{/when}}',
  '{{#when::var::toString}}P{{:else}}p{{/when}}|{{#when::toggle::toString}}Q{{:else}}q{{/when}}',
]);
add('when-no-character', ['blockStartMatcher:case:var', 'blockStartMatcher:case:vis'], 'noCharacter',
  ['{{#when::var::mood}}V{{:else}}v{{/when}}|{{#when::mood::vis::null}}N{{/when}}']);
add('declare', ['declare'], 'full', ['{{declare::x}}|{{declare}}']);

// Display names: the viewport is the page's, and the doc-only asset names stay for the display pipeline.
add('screen-size', ['screenwidth', 'screenheight'], 'full', ['{{screenwidth}}x{{screenheight}}|{{screen_width}}x{{screen_height}}']);
add('display-preserved', ['asset', 'emotion', 'audio', 'bg', 'bgm', 'video', 'video-img', 'image', 'img', 'path', 'inlay', 'inlayed', 'inlayeddata', 'source'], 'full', [
  { text: '{{asset::a}}{{emotion::b}}{{audio::c}}{{bg::d}}{{bgm::e}}{{video::f}}{{video-img::g}}{{image::h}}{{img::i}}{{path::j}}{{inlay::k}}{{inlayed::l}}{{inlayeddata::m}}{{source::n}}', visualize: true },
  '{{asset::a}}{{img::i}}{{inlay::k}}{{source::user}}',
]);

// Random draws and hash picks.
add('random', ['random'], 'full', ['{{random}}|{{random::a::b::c}}|{{random::x,y\\,z}}|{{random::[1,"two",null]}}|{{random::p:q}}'],
  { tape: [0.25, 0.5, 0.99, 0.4, 0.6] });
add('random-tokenize', ['random'], 'full', ['{{random::a::b}}|{{random::x,y}}'], { tape: [0.9, 0.9], tokenizeAccurate: true });
add('randint-dice-roll', ['randint', 'dice', 'roll'], 'full',
  ['{{randint::1::6}}|{{dice::2d6}}|{{roll::2d6}}|{{roll}}|{{roll::20}}|{{roll::0d6}}|{{randint::a::b}}'], { tape: [0.5, 0.1, 0.9, 0.3, 0.7, 0.95] });
add('pick', ['pick'], 'full', ['{{pick}}|{{pick::a,b,c}}|{{pick::[1,2,3]}}|{{pick::x::y}}|{{pick::a\\,b,c}}']);
add('pick-tokenize', ['pick'], 'full', ['{{pick::a,b,c}}'], { tokenizeAccurate: true });
add('pick-empty-chat', ['pick'], 'emptyChat', ['{{pick}}|{{pick::a,b,c}}']);
add('pick-no-character', ['pick', 'rollp'], 'noCharacter', ['{{pick}}|{{rollp::2d6}}']);
add('rollp', ['rollp'], 'full', ['{{rollp::3d6}}|{{rollpick::20}}|{{rollp}}|{{rollp::0}}|{{rollp::d}}|{{rollp::x}}']);
add('rollp-empty-chat', ['rollp'], 'emptyChat', ['{{rollp::2d10}}']);

// Lorebooks, modules, assets and model metadata.
add('lorebook', ['lorebook'], 'library', ['{{lorebook}}', '{{worldinfo}}']);
add('lorebook-empty', ['lorebook'], 'full', ['{{lorebook}}']);
add('lorebook-no-character', ['lorebook'], 'noCharacter', ['[{{lorebook}}]']);
add('modules', ['moduleenabled', 'moduleassetlist'], 'library', [
  '{{moduleenabled::ns.a}}|{{moduleenabled::ns.c}}|{{moduleenabled::mod-a}}|{{moduleenabled::ns.x}}|{{moduleenabled}}|{{module_enabled::ns.a}}',
  '{{moduleassetlist::ns.a}}|[{{moduleassetlist::ns.c}}]|[{{moduleassetlist::mod-a}}]|{{module_assetlist::ns.a}}|[{{moduleassetlist}}]',
]);
add('modules-none', ['moduleenabled', 'moduleassetlist'], 'full', ['{{moduleenabled::ns.a}}|{{moduleenabled}}|[{{moduleassetlist::ns.a}}]|[{{moduleassetlist}}]']);
for (const snapshot of ['library', 'library2', 'full', 'noCharacter'])
  add(`assets-${snapshot}`, ['emotionlist', 'assetlist', 'chardisplayasset'], snapshot, ['[{{emotionlist}}|{{assetlist}}|{{chardisplayasset}}]']);
for (const snapshot of ['full', 'claude', 'library', 'legacy', 'anthropicPlain'])
  add(`prefill-${snapshot}`, ['prefillsupported'], snapshot, ['{{prefillsupported}}|{{prefill_supported}}|{{prefill}}']);
const metadataKeys = ['mobile', 'local', 'node', 'version', 'majorversion', 'majorver', 'major', 'language', 'locale', 'lang',
  'browserlanguage', 'browserlocale', 'browserlang', 'modelshortname', 'modelname', 'modelinternalid', 'modelformat', 'modelprovider',
  'modeltokenizer', 'imateapot', 'risutype', 'maxcontext', 'VERSION', 'Lang', 'bogus', ''];
const metadataText = metadataKeys.map(key => `{{metadata::${key}}}`).join('|');
add('metadata', ['metadata'], 'full', [metadataText, '[{{metadata}}]']);
add('metadata-custom', ['metadata'], 'library', [metadataText], { profile: 'newYork' });
add('metadata-models', ['metadata'], 'legacy', ['{{metadata::modelname}}|{{metadata::modelshortname}}|{{metadata::modelinternalid}}|{{metadata::modelformat}}']);
add('metadata-models-plain', ['metadata'], 'anthropicPlain', ['{{metadata::modelinternalid}}|{{metadata::modelformat}}']);

// The trigger id the arg carries once it sets currentTime.
add('trigger-id-current-time', ['trigger_id'], 'full', [
  { text: '{{trigger_id}}', currentTime: 1767225600000 }, { text: '{{trigger_id}}', currentTime: 1767225600000, triggerId: 'arg-trigger' }, '{{trigger_id}}',
]);

// History windows: the windowed conversation's loaded messages start at absolute index 10.
add('window-indices', ['lastmessageid', 'previouschatlog', 'history', 'role', 'previouscharchat', 'previoususerchat', 'pick', 'rollp', 'messagetime', 'idleduration'], 'windowed', [
  '{{lastmessageid}}|{{previouschatlog::12}}|{{previouschatlog::2}}|{{previouschatlog::14}}|{{pick::a,b,c,d}}|{{rollp::2d6}}|{{idleduration}}',
  { text: '{{role}}|{{previouscharchat}}|{{previoususerchat}}|{{messagetime}}', chatIndex: 12 },
  { text: '[{{role}}]|{{previouscharchat}}|{{previoususerchat}}', chatIndex: 3 },
  { text: '{{previouscharchat}}|{{previoususerchat}}', chatIndex: 9 },
  '{{history}}', '{{history::role}}',
  { text: '{{previouschatlog::12}}|{{role}}|{{previouscharchat}}', chatIndex: 3, projectedChatID: 13, historyOffset: 2 },
  { text: '[{{previoususerchat}}]|[{{role}}]', chatIndex: 12, projectedChatID: -1 },
]);
add('window-zero', ['history', 'lastmessageid'], 'windowedZero', ['{{history}}|{{lastmessageid}}']);
add('projection-unwindowed', ['previouschatlog', 'role'], 'full', [{ text: '{{previouschatlog::3}}|{{role}}', chatIndex: 0, projectedChatID: 2, historyOffset: 1 }]);

// Prepared history access: rows, complete histories and searches, and a miss that throws out of the parse.
const row = (role, data, extra = {}) => ({ role, data, ...extra });
const complete = [row('char', 'C0', { time: 1000 }), row('user', 'C1 {{char}}', { time: 61000 }), row('user', 'C2', { time: 3723000 })];
add('access-rows', ['previouschatlog', 'lastmessage', 'lastmessageid', 'role', 'pick'], 'full', [
  { text: '{{previouschatlog::1}}|{{previouschatlog::-0}}|{{previouschatlog::9}}|{{previouschatlog::-1}}|{{previouschatlog::1.5}}|{{lastmessageid}}|{{lastmessage}}|{{pick::a,b,c,d}}',
    history: { length: 9, rows: [[0, row('char', 'P0')], [1, row('user', 'P1')], [8, row('char', 'P8')]] } },
  { text: '{{role}}', chatIndex: 1, history: { length: 9, rows: [[1, row('user', 'P1')]] } },
  { text: '{{previouschatlog::2}}', history: { length: 9, rows: [[1, row('user', 'P1')]] }, refusal: 'history-unprepared' },
]);
add('access-no-character', ['previouschatlog', 'role', 'lastmessage'], 'noCharacter', [
  { text: '{{previouschatlog::0}}|{{role}}|[{{lastmessage}}]', chatIndex: 0, history: { length: 1, rows: [[0, row('user', 'N0')]] } },
]);
add('access-complete', ['history', 'userhistory', 'messageunixtimearray', 'previouscharchat', 'messageidleduration', 'idleduration'], 'full', [
  { text: '{{history}}|{{userhistory}}|{{messageunixtimearray}}', history: { length: 3, complete } },
  { text: '{{previouscharchat}}|{{messageidleduration}}|{{idleduration}}', chatIndex: 2, history: { length: 3, complete } },
]);
add('access-searches', ['previouscharchat', 'previoususerchat', 'messageidleduration'], 'full', [
  { text: '{{previouscharchat}}|{{previoususerchat}}', chatIndex: 3, history: { length: 9, searches: [['char', 3, 1, [row('char', 'S-char')]], ['user', 3, 1, []]] } },
  { text: '{{messageidleduration}}', chatIndex: 4, history: { length: 9, searches: [['user', 5, 2, [row('user', 'late', { time: 7200000 }), row('user', 'early', { time: 1 })]]] } },
  { text: '{{previouscharchat}}', chatIndex: 5, history: { length: 9, searches: [] }, refusal: 'history-unprepared' },
]);
add('access-all-unprepared', ['history'], 'full', [{ text: '{{history::role}}', history: { length: 5, rows: [] }, refusal: 'history-unprepared' }]);

// Message times in each profile's zone and locale.
for (const profile of ['utc', 'newYork', 'lordHowe'])
  add(`messagetime-${profile}`, ['messagetime', 'messagedate'], 'clocked',
    [0, 1, 2, 3, 4, 5, 6, 9].map(chatIndex => ({ text: '{{messagetime}}|{{messagedate}}|{{message_time}}|{{message_date}}', chatIndex })), { profile });
add('messagetime-flags', ['messagetime', 'messagedate'], 'clocked', [
  '{{messagetime}}|{{messagedate}}', { text: '{{messagetime}}|{{messagedate}}', chatIndex: 7 }, { text: '[{{messagetime}}|{{messagedate}}]', chatIndex: 20 },
]);
add('messagetime-tokenize', ['messagetime', 'messagedate', 'messageidleduration', 'idleduration'], 'clocked',
  [{ text: '{{messagetime}}|{{messagedate}}|{{messageidleduration}}|{{idleduration}}', chatIndex: 2 }], { tokenizeAccurate: true });
add('messagetime-no-character', ['messagetime', 'messagedate', 'messageidleduration', 'idleduration'], 'noCharacter',
  [{ text: '[{{messagetime}}|{{messagedate}}|{{messageidleduration}}|{{idleduration}}]', chatIndex: 0 }]);
add('messagetime-string', ['messagetime'], 'clocked', [{ text: '{{messagetime}}', chatIndex: 8, refusal: 'time-value' }]);
add('messagetime-cairo', ['messagetime'], 'clocked', [{ text: '{{messagetime}}', chatIndex: 0, refusal: 'locale-format' }], { profile: 'cairo' });
add('messagedate-cairo', ['messagedate'], 'clocked', [{ text: '{{messagedate}}', chatIndex: 0, refusal: 'locale-format' }], { profile: 'cairo' });
add('messageidleduration', ['messageidleduration'], 'clocked',
  [2, 5, 0, 1, 7, 9, -1, -5, 30].map(chatIndex => ({ text: '[{{messageidleduration}}|{{message_idle_duration}}]', chatIndex })));
add('idleduration', ['idleduration'], 'clocked', ['{{idleduration}}|{{idle_duration}}', { text: '{{idleduration}}', currentTime: 1800000000000 }]);
add('idleduration-full', ['idleduration'], 'full', ['{{idleduration}}']);
add('idleduration-empty', ['idleduration'], 'emptyChat', ['{{idleduration}}']);
add('idleduration-untimed', ['idleduration'], 'lastUntimed', ['{{idleduration}}']);

// The clock: Date.now() at the case clock, or the arg's currentTime, in each profile's zone. {{time}} runs the
// second time registration, which shadows the first.
const clockText = '{{unixtime}}|{{time}}|{{isotime}}|{{isodate}}|{{date}}|{{date::YYYY-MM-DD HH:mm:ss hh A DDDD dddd x X}}';
const clockFamilies = ['unixtime', 'time@2', 'isotime', 'isodate', 'date'];
for (const profile of Object.keys(hostProfiles)) add(`clock-${profile}`, clockFamilies, 'full', [clockText], { profile });
for (const [profile, clocks] of [
  ['newYork', [1772953199999, 1772953200000, 1793512799999, 1793512800000, 1782880200000]],
  ['lordHowe', [1775314799999, 1775314800000, 1791041399999, 1791041400000]],
  ['cairo', [1776981599999, 1776981600000, 1793307599999, 1793307600000]],
]) for (const clock of clocks) add(`clock-${profile}-${clock}`, clockFamilies, 'full', [clockText], { profile, clock });
add('clock-current-time', clockFamilies, 'full', [
  { text: clockText, currentTime: 1782880200500 }, { text: clockText, currentTime: 1782880200499.9 }, { text: clockText, currentTime: -1500 },
  { text: '{{unixtime}}|{{date::X}}|{{time::HH:mm:ss}}', currentTime: 0 }, { text: '{{unixtime}}|{{time}}|{{isodate}}|{{date}}|[{{date::YYYY}}]', currentTime: 1e300 },
], { profile: 'newYork' });
add('datetimeformat', ['date', 'time@2'], 'full', [
  '{{date::YYYY-MM-DD HH:mm:ss}}|{{time::hh:mm A}}|{{datetimeformat::YY/M/D}}|{{time::YYYY}}',
  '{{date::MMMM MMM MM DDDD DD dddd ddd}}|{{date:::YYYY}}|[{{date::}}]|{{date::X x}}',
  '{{date::MMMM dddd A::6307200000000}}|{{date::MMMM::19526400000000}}|{{date::HH:mm::abc}}|{{date::YYYY::0}}|{{date::YYYY::-0}}',
  `[{{date::${'Y'.repeat(301)}}}]|{{date:::${'Y'.repeat(300)}}}`,
  '[{{date::YYYY::1e30}}]|{{time::HH}}',
]);
add('datetimeformat-names', ['date'], 'full',
  [[0, 1], [1, 2], [2, 3], [3, 4], [4, 5], [5, 6], [6, 7], [7, 8], [8, 9], [9, 10], [10, 11], [11, 12], [0, 4], [0, 2]]
    .map(([month, day]) => `{{date::MMMM MMM dddd ddd DDDD::${Date.UTC(1970, month, day) * 1000}}}`));
add('datetimeformat-zones', ['date'], 'full', ['{{date::MMMM YYYY DDDD HH A}}|{{date::MMMM DDDD HH::19526400000000}}'], { profile: 'newYork' });
add('datetimeformat-zone-range', ['date'], 'full', [{ text: '{{date::YYYY::-3000000000000000}}', refusal: 'time-zone-range' }]);
add('clock-zone-range', ['time@2'], 'full', [{ text: '{{time}}', currentTime: -3000000000000, refusal: 'time-zone-range' }]);

export const hostCases = cases;

// The product sources the host oracle executes beyond the shared parser sources.
export const hostOracleSources = [
  ...oracleSources, 'src/ts/parser/chatVar.svelte.ts', 'src/ts/storage/database.svelte.ts', 'src/ts/process/historyWindowIndex.ts',
  'src/ts/process/scriptCharacterLookup.ts', 'src/ts/safeStructuredClone.ts', 'src/ts/cbsHistoryAccess.ts', 'src/ts/process/modules.ts',
  'src/ts/process/moduleSelection.ts', 'src/ts/model/modellist.ts', 'src/ts/model/types.ts', 'src/ts/platform.ts', 'src/ts/appVersion.ts',
];
