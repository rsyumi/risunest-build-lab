import assert from 'node:assert/strict';
import { createCipheriv, createHash } from 'node:crypto';
import { createReadStream, createWriteStream } from 'node:fs';
import { mkdir, open, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pipeline } from 'node:stream/promises';

// The browser fixture's no-eval encoder must use its browser buffer path on Node too.
const nativeBuffer = globalThis.Buffer;
let fixture;
let Packr;
try {
  globalThis.Buffer = undefined;
  ({ Packr } = await import('msgpackr/index-no-eval'));
  fixture = await import('./fixture.ts');
} finally { globalThis.Buffer = nativeBuffer; }

const [axis, sizeText, destination] = process.argv.slice(2);
const size = Number(sizeText);
assert.ok(['encrypted-database', 'attachment', 'record'].includes(axis));
assert.ok((axis === 'record' ? [1, 8, 32] : [100, 300, 600]).includes(size));
assert.ok(destination, 'Pass a new synthetic fixture directory');
const directory = path.resolve(destination);
await mkdir(directory);
const source = path.join(directory, 'source.bin');
const database = path.join(directory, 'database.risudat');
const key = 'synthetic backup key 한국어';
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const pack = new Packr({ useRecords: false });
const header = Buffer.from([0, 82, 73, 83, 85, 83, 65, 86, 69, 0, 7]);
const expected = [];
const remember = character => {
  const chat = character.chats[0];
  expected.push({ characterId: character.chaId, conversationId: chat.id, messages: chat.message.length,
    projectionSha256: hash(fixture.messageProjection(chat.message)) });
};
const file = await open(database, 'wx');
try {
  await file.writeFile(header);
  if (axis === 'record') {
    const character = { type: 'character', chaId: 'synthetic-000000', name: 'Synthetic record', chats: [
      { id: 'chat-000000', name: 'Synthetic chat', message: [{ role: 'user', chatId: 'synthetic-message', data: 'r'.repeat(size * 1024 * 1024) }] },
    ] };
    remember(character);
    await file.writeFile(pack.encode({ username: 'Synthetic memory fixture', characters: [character], botPresets: [] }));
  } else {
    const plan = fixture.planFixture(axis === 'encrypted-database' ? size * 1_000_000 : 2_500_000);
    for (const packet of fixture.fixturePackets(plan)) await file.writeFile(packet);
    for (let index = 0; index < plan.characterCount; index++) remember(fixture.fixtureCharacter(plan, index));
  }
} finally { await file.close(); }

let databaseSource = database;
if (axis === 'encrypted-database') {
  databaseSource = path.join(directory, 'encrypted.risudat');
  const cipher = createCipheriv('aes-256-gcm', createHash('sha256').update(key).digest(), Buffer.alloc(12));
  await pipeline(createReadStream(database, { highWaterMark: 64 * 1024 }), cipher, createWriteStream(databaseSource, { flags: 'wx' }));
  const output = await open(databaseSource, 'a');
  try { await output.writeFile(cipher.getAuthTag()); } finally { await output.close(); }
}
const archive = await open(source, 'wx');
const addEntry = async (name, file) => {
  const encoded = Buffer.from(name);
  const header = Buffer.alloc(8 + encoded.length);
  header.writeUInt32LE(encoded.length);
  encoded.copy(header, 4);
  header.writeUInt32LE((await stat(file)).size, 4 + encoded.length);
  await archive.writeFile(header);
  for await (const chunk of createReadStream(file, { highWaterMark: 64 * 1024 })) await archive.writeFile(chunk);
};
let attachment;
try {
  await addEntry('database.risudat', databaseSource);
  if (axis === 'encrypted-database') {
    const metadata = path.join(directory, 'encryption.risudat');
    await writeFile(metadata, JSON.stringify({ type: 'account', time: 1 }));
    await addEntry('encryption.risudat', metadata);
  }
  if (axis === 'attachment') {
    const body = path.join(directory, 'synthetic-attachment.bin');
    const output = await open(body, 'wx');
    const digest = createHash('sha256');
    const chunk = Buffer.alloc(64 * 1024, 0x5a);
    try {
      let left = size * 1_000_000;
      while (left > 0) {
        const bytes = chunk.subarray(0, Math.min(left, chunk.length));
        await output.writeFile(bytes);
        digest.update(bytes);
        left -= bytes.length;
      }
    } finally { await output.close(); }
    await addEntry('synthetic-attachment.bin', body);
    attachment = { bytes: size * 1_000_000, sha256: digest.digest('hex') };
  }
} finally { await archive.close(); }
const sourceDigest = createHash('sha256');
for await (const chunk of createReadStream(source)) sourceDigest.update(chunk);
const manifest = { schema: 'risunest.synthetic-restore-resources/v1', synthetic: true, axis, size,
  source: 'source.bin', sourceBytes: (await stat(source)).size, sourceSha256: sourceDigest.digest('hex'),
  databaseBytes: (await stat(database)).size, key: axis === 'encrypted-database' ? key : undefined, expected, attachment };
await writeFile(path.join(directory, 'manifest.json'), JSON.stringify(manifest, null, 2));
console.log(JSON.stringify({ axis, size, sourceBytes: manifest.sourceBytes, databaseBytes: manifest.databaseBytes, characters: expected.length }));
