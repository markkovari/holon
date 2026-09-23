// Helpers for the photoquest suite (tests/photoquest*.spec.js, photoquest.sh):
// the app's API for setting a scenario up — accounts, uploads, and the game
// (the bootstrap admin, curators, journeys, competitions, the test clock).
//
// Everything here talks to the app the way the browser does — register, log in,
// ask for an upload plan, PUT each part straight to its presigned URL, complete —
// but through a Playwright APIRequestContext, so a spec can set up a second user
// or a queue of photos without clicking through the page for each one.
//
// Also: the CC0 samples. The suite never ships media; photoquest.sh downloads the
// two raw.pixls.us a7R V ARWs into e2e/.photoquest-samples/ (gitignored) and
// `derivePreviewJpeg` cuts a small JPEG out of one of them, so every byte the
// suite uploads is CC0.
//
//   node lib/photoquest.js derive-jpeg <in.ARW> <out.jpg>
//   node lib/photoquest.js warm <file> [base]      one upload, waits for `evaluated`

const fs = require('fs');
const path = require('path');

const BASE = process.env.PHOTOQUEST_URL || 'http://127.0.0.1:3941';
const MEDIA = process.env.PHOTOQUEST_MEDIA_URL || 'http://127.0.0.1:8013';
const SAMPLES = process.env.PHOTOQUEST_SAMPLES || path.join(__dirname, '..', '.photoquest-samples');

const SAMPLE = {
  uncompressed: path.join(SAMPLES, '7RM5-LosslessUncompressed.ARW'),
  compressed: path.join(SAMPLES, '7RM5-LosslessCompressedLarge.ARW'),
  jpeg: path.join(SAMPLES, '7RM5-preview.jpg'),
};

// ---- the ARW / JPEG bytes ---------------------------------------------------

function tiffReader(buf, base) {
  const le = buf.toString('latin1', base, base + 2) === 'II';
  const u16 = (o) => (le ? buf.readUInt16LE(base + o) : buf.readUInt16BE(base + o));
  const u32 = (o) => (le ? buf.readUInt32LE(base + o) : buf.readUInt32BE(base + o));
  // One IFD as { tag: {type, count, value} }, `value` the raw 4-byte field
  // (an offset for anything that does not fit in it).
  const ifd = (off) => {
    const tags = {};
    const n = u16(off);
    for (let i = 0; i < n; i++) {
      const e = off + 2 + 12 * i;
      tags[u16(e)] = { type: u16(e + 2), count: u32(e + 4), value: u32(e + 8) };
    }
    return { tags, next: u32(off + 2 + 12 * n) };
  };
  return { ifd, first: u32(4) };
}

/// The JPEG preview a Sony ARW carries in IFD0 (JPEGInterchangeFormat /
/// ...Length, 0x201/0x202): about 1616x1080, half a megabyte, no EXIF of its
/// own. Cut out of a CC0 file, it is CC0 too.
function derivePreviewJpeg(arw, out) {
  const buf = fs.readFileSync(arw);
  const t = tiffReader(buf, 0);
  const { tags } = t.ifd(t.first);
  if (!tags[0x201] || !tags[0x202]) throw new Error(`${arw}: no JPEG preview in IFD0`);
  const jpg = buf.subarray(tags[0x201].value, tags[0x201].value + tags[0x202].value);
  if (jpg[0] !== 0xff || jpg[1] !== 0xd8) throw new Error(`${arw}: IFD0 preview is not a JPEG`);
  fs.writeFileSync(out, jpg);
}

/// The EXIF tags that say how the pixels are laid out and nothing about who,
/// with what, or where: dimensions, resolution, orientation, colour space, the
/// EXIF version, the pointer to the EXIF IFD. Core Image writes a block of
/// exactly these into every JPEG even with every property removed.
const STRUCTURAL_EXIF = new Set([
  0x0100, 0x0101, 0x0102, 0x0103, 0x0106, 0x0112, 0x0115, 0x011a, 0x011b, 0x0128,
  0x0201, 0x0202, 0x0211, 0x0212, 0x0213, 0x0214, 0x8769,
  0x9000, 0x9101, 0xa000, 0xa001, 0xa002, 0xa003, 0xa406,
]);

/// What a JPEG carries besides pixels, by walking its marker segments (no
/// dependency — e2e/ has only @playwright/test). Returns the APP segments found,
/// every EXIF tag in every IFD of an APP1 Exif segment, `unexpected`: the tags
/// not in STRUCTURAL_EXIF (an allow-list, so Make/Model/lens/dates count too),
/// and `identifying`: the ones that name a camera or a place outright — 0x8825
/// GPS IFD, 0xA431 BodySerialNumber, 0x927C MakerNote (Sony keeps the serial in
/// there, encrypted, so a byte search for it proves nothing; the MakerNote's
/// absence does), 0xC634 DNGPrivateData (Sony's SR2 block).
function jpegMetadata(buf) {
  if (buf[0] !== 0xff || buf[1] !== 0xd8) throw new Error('not a JPEG');
  const out = { app: [], exifTags: [] };
  let o = 2;
  while (o + 4 <= buf.length) {
    if (buf[o] !== 0xff) break;
    const marker = buf[o + 1];
    if (marker === 0xda || marker === 0xd9) break; // start of scan: metadata is over
    const len = buf.readUInt16BE(o + 2);
    const seg = buf.subarray(o + 4, o + 2 + len);
    if (marker >= 0xe0 && marker <= 0xef) {
      const id = seg.toString('latin1', 0, Math.min(seg.length, 29)).split('\0')[0];
      out.app.push(`APP${marker - 0xe0}:${id}`);
      if (marker === 0xe1 && id === 'Exif') {
        const tiff = Buffer.from(seg.subarray(6));
        const t = tiffReader(tiff, 0);
        const walk = (off, seen) => {
          if (!off || seen.has(off) || off >= tiff.length) return;
          seen.add(off);
          const { tags, next } = t.ifd(off);
          for (const k of Object.keys(tags)) out.exifTags.push(Number(k));
          if (tags[0x8769]) walk(tags[0x8769].value, seen);
          walk(next, seen);
        };
        walk(t.first, new Set());
      }
    }
    o += 2 + len;
  }
  out.identifying = out.exifTags.filter((t) => [0x8825, 0xa431, 0x927c, 0xc634].includes(t));
  out.unexpected = out.exifTags.filter((t) => !STRUCTURAL_EXIF.has(t));
  return out;
}

// ---- the app's API ------------------------------------------------------------

let seq = 0;
function uniqueEmail(who = 'photographer') {
  return `${who}-${Date.now().toString(36)}-${process.pid}-${seq++}@example.com`;
}

async function json(res) {
  const text = await res.text();
  try { return text ? JSON.parse(text) : null; } catch (_) { return { raw: text }; }
}

/// Register and log in a fresh photographer; returns { email, password, token }.
async function signUp(request, who) {
  const email = uniqueEmail(who), password = 'correct horse battery';
  const reg = await request.post(`${BASE}/register`, { data: { email, password } });
  const body = await json(reg);
  if (reg.status() !== 201) throw new Error(`register ${reg.status()}: ${JSON.stringify(body)}`);
  const token = await logIn(request, email, password);
  return { email, password, token, subject: body.subject };
}

async function logIn(request, email, password) {
  const res = await request.post(`${BASE}/login`, { data: { email, password } });
  if (res.status() !== 200) throw new Error(`login ${res.status()}`);
  return (await json(res)).access_token;
}

const auth = (token) => ({ Authorization: `Bearer ${token}` });

/// The same CC0 picture as different bytes, so it has its own sha256 — the game
/// pays XP once per file, ever, and refuses a second entry of the same file, so
/// a suite that needs many "different" photos gets them this way. A JPEG gets a
/// COM segment after SOI; anything else (an ARW: TIFF, every offset from the
/// start) gets the salt appended after its last byte. Pixels and metadata are
/// untouched, so the evaluation is the same as the unsalted file's.
function salted(buf, salt) {
  if (!salt) return buf;
  const tag = Buffer.from(`photoquest-e2e ${salt}`, 'latin1');
  if (buf[0] === 0xff && buf[1] === 0xd8) {
    const seg = Buffer.alloc(4);
    seg.writeUInt16BE(0xfffe, 0);
    seg.writeUInt16BE(tag.length + 2, 2);
    return Buffer.concat([buf.subarray(0, 2), seg, tag, buf.subarray(2)]);
  }
  return Buffer.concat([buf, tag]);
}

let saltSeq = 0;
/// A salt no other upload in this run uses.
function freshSalt() {
  return `${Date.now().toString(36)}-${process.pid}-${saltSeq++}`;
}

/// Upload a file the way app.js does, and complete it. Returns the photo id.
/// Does not wait for the evaluation. With `complete: false` it stops after the
/// parts and returns `{ id, parts }` for `complete` later. `salt: true` (or a
/// string) makes it a file of its own — see `salted`.
async function upload(request, token, file, { filename, contentType = '', complete: finish = true, salt } = {}) {
  const buf = salted(fs.readFileSync(file), salt === true ? freshSalt() : salt);
  const created = await request.post(`${BASE}/api/photos`, {
    headers: auth(token),
    data: { filename: filename || path.basename(file), size: buf.length, content_type: contentType },
  });
  const body = await json(created);
  if (created.status() !== 201) throw new Error(`create ${created.status()}: ${JSON.stringify(body)}`);
  const { photo, upload: plan } = body;
  const parts = [];
  for (const part of plan.parts) {
    const start = (part.number - 1) * plan.part_size;
    const res = await request.put(part.url, { data: buf.subarray(start, Math.min(start + plan.part_size, buf.length)) });
    if (!res.ok()) throw new Error(`part ${part.number}: ${res.status()}`);
    parts.push({ number: part.number, etag: res.headers()['etag'] });
  }
  if (!finish) return { id: photo.id, parts };
  await complete(request, token, { id: photo.id, parts });
  return photo.id;
}

async function complete(request, token, { id, parts }) {
  const done = await request.post(`${BASE}/api/photos/${id}/complete`, { headers: auth(token), data: { parts } });
  if (done.status() !== 200) throw new Error(`complete ${done.status()}: ${JSON.stringify(await json(done))}`);
}

async function getPhoto(request, token, id) {
  const res = await request.get(`${BASE}/api/photos/${id}`, { headers: auth(token) });
  return { status: res.status(), body: await json(res) };
}

async function listPhotos(request, token) {
  const res = await request.get(`${BASE}/api/photos`, { headers: auth(token) });
  return (await json(res)).photos;
}

/// Poll until the photo settles; returns the record. Throws on `failed`.
async function waitSettled(request, token, id, timeoutMs = 120000) {
  const until = Date.now() + timeoutMs;
  for (;;) {
    const { status, body } = await getPhoto(request, token, id);
    if (status !== 200) throw new Error(`GET photo ${status}`);
    if (body.state === 'evaluated') return body;
    if (body.state === 'failed') throw new Error(`evaluation failed: ${body.error}`);
    if (Date.now() > until) throw new Error(`still ${body.state} after ${timeoutMs} ms`);
    await new Promise((r) => setTimeout(r, 500));
  }
}

/// Upload, wait for `evaluated`, return the record.
async function evaluatedPhoto(request, token, file, opts = {}) {
  const id = await upload(request, token, file, opts);
  return waitSettled(request, token, id);
}

// ---- the game's API (quests, competitions, moderation) --------------------------

/// The account photoquest.sh names in config `bootstrap-admin-email`: admin
/// from the moment it registers. Its password is the suite's own.
const ADMIN_EMAIL = process.env.PHOTOQUEST_ADMIN_EMAIL || 'admin@photoquest.test';
const ADMIN_PASSWORD = 'photoquest e2e admin';

/// One JSON call: `{ status, body }`, never throws on a non-2xx.
async function call(request, token, method, pathname, data) {
  const res = await request.fetch(`${BASE}${pathname}`, {
    method,
    headers: token ? auth(token) : {},
    data: data === undefined ? undefined : data,
  });
  return { status: res.status(), body: await json(res) };
}

/// Like `call`, but a status other than `expect` is an error with the body in it.
async function must(request, token, method, pathname, data, expect = [200, 201]) {
  const r = await call(request, token, method, pathname, data);
  const ok = Array.isArray(expect) ? expect.includes(r.status) : r.status === expect;
  if (!ok) throw new Error(`${method} ${pathname} → ${r.status}: ${JSON.stringify(r.body)}`);
  return r.body;
}

/// The bootstrap admin's `{ email, password, token, subject }` — registered by
/// whichever test asks first in this run, logged in after that.
async function admin(request) {
  const reg = await request.post(`${BASE}/register`, { data: { email: ADMIN_EMAIL, password: ADMIN_PASSWORD } });
  const token = await logIn(request, ADMIN_EMAIL, ADMIN_PASSWORD);
  const me = await must(request, token, 'GET', '/me');
  if (!me.roles.includes('admin')) {
    throw new Error(`${ADMIN_EMAIL} is not admin (register ${reg.status()}) — is config bootstrap-admin-email set? photoquest.sh sets it`);
  }
  return { email: ADMIN_EMAIL, password: ADMIN_PASSWORD, token, subject: me.subject };
}

async function grant(request, adminToken, subject, role) {
  return must(request, adminToken, 'POST', `/api/admin/users/${subject}/roles`, { grant: role });
}

/// A fresh account with the curator role, granted by the bootstrap admin.
async function curator(request, who = 'curator') {
  const u = await signUp(request, who);
  await grant(request, (await admin(request)).token, u.subject, 'curator');
  return u;
}

/// Shift the app's clock (config `allow-test-routes`). Returns the app's now.
async function setClock(request, offset_secs) {
  return (await must(request, null, 'POST', '/test/clock', { offset_secs })).now;
}

const nowSecs = () => Math.floor(Date.now() / 1000);

/// A published journey with published quests, in order. `quests` are the
/// POST /api/curator/quests bodies without `journey`. Returns
/// `{ journey, quests: [quest records] }`.
async function publishedJourney(request, curatorToken, { title, levels, badge = null, description = '' }, quests) {
  const journey = await must(request, curatorToken, 'POST', '/api/curator/journeys', { title, description, levels, badge });
  const made = [];
  for (const q of quests) {
    const quest = await must(request, curatorToken, 'POST', '/api/curator/quests', { journey: journey.id, ...q });
    await must(request, curatorToken, 'POST', `/api/curator/quests/${quest.id}/publish`);
    made.push(quest);
  }
  await must(request, curatorToken, 'POST', `/api/curator/journeys/${journey.id}/publish`);
  return { journey, quests: made };
}

/// A published competition. Defaults: opens a minute ago, entries close in an
/// hour, voting a day later, judging two; auto-only scoring, one entry each,
/// no prizes. Returns the record.
async function publishedCompetition(request, curatorToken, fields = {}) {
  const t = nowSecs();
  const c = await must(request, curatorToken, 'POST', '/api/curator/competitions', {
    title: 'Competition', brief: '', requirements: {},
    opens_at: t - 60, closes_at: t + 3600, voting_closes_at: t + 86400, judging_closes_at: t + 2 * 86400,
    weights: { auto: 1, votes: 0, judges: 0 }, max_entries_per_user: 1, prizes_xp: [],
    ...fields,
  });
  return must(request, curatorToken, 'POST', `/api/curator/competitions/${c.id}/publish`);
}

/// What the real pipeline reports for every CC0 sample (ARW and JPEG alike, on
/// the Vision backend): a `grass` label at ~0.86–0.90, focus ratio 7.2–9.2, no
/// face. The 2022 capture date would fail `captured_after_start`, so it is off
/// — except where a scenario tests exactly that.
const GRASS = { subject: { label: 'grass', min_confidence: 0.5 }, sharpness: { min_focus_ratio: 5 }, captured_after_start: false };

/// Requirements every CC0 sample passes on this box: GRASS where Vision runs
/// (comp-media's /health says `apple`), and without the label where it does
/// not — on the CPU path a label check is "not looked at", which never passes.
function passable(health) {
  if (health && health.apple) return GRASS;
  const { subject, ...rest } = GRASS; // eslint-disable-line no-unused-vars
  return rest;
}

/// comp-media's /health: which backend this box evaluates on.
async function mediaHealth(request) {
  const res = await request.get(`${MEDIA}/health`);
  return json(res);
}

module.exports = {
  BASE, MEDIA, SAMPLES, SAMPLE,
  derivePreviewJpeg, jpegMetadata,
  uniqueEmail, signUp, logIn, auth, upload, complete, getPhoto, listPhotos, waitSettled, mediaHealth,
  salted, freshSalt, evaluatedPhoto,
  ADMIN_EMAIL, call, must, admin, grant, curator, setClock, nowSecs, publishedJourney, publishedCompetition, GRASS, passable,
};

// ---- CLI (photoquest.sh) --------------------------------------------------------

if (require.main === module) {
  const [cmd, ...args] = process.argv.slice(2);
  (async () => {
    if (cmd === 'derive-jpeg') {
      derivePreviewJpeg(args[0], args[1]);
    } else if (cmd === 'warm') {
      const { request } = require('@playwright/test');
      const ctx = await request.newContext();
      const t0 = Date.now();
      const u = await signUp(ctx, 'warmup');
      const id = await upload(ctx, u.token, args[0]);
      const p = await waitSettled(ctx, u.token, id, 300000);
      console.log(`warm: ${path.basename(args[0])} evaluated in ${Date.now() - t0} ms by ${JSON.stringify(p.backend)}`);
      await ctx.dispose();
    } else {
      console.error('usage: photoquest.js derive-jpeg <in.ARW> <out.jpg> | warm <file>');
      process.exit(2);
    }
  })().catch((e) => { console.error(e.message || e); process.exit(1); });
}
