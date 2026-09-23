// Helpers for the photoquest suite (tests/photoquest.spec.js, photoquest.sh).
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
  if (reg.status() !== 201) throw new Error(`register ${reg.status()}: ${JSON.stringify(await json(reg))}`);
  const token = await logIn(request, email, password);
  return { email, password, token };
}

async function logIn(request, email, password) {
  const res = await request.post(`${BASE}/login`, { data: { email, password } });
  if (res.status() !== 200) throw new Error(`login ${res.status()}`);
  return (await json(res)).access_token;
}

const auth = (token) => ({ Authorization: `Bearer ${token}` });

/// Upload a file the way app.js does, and complete it. Returns the photo id.
/// Does not wait for the evaluation. With `complete: false` it stops after the
/// parts and returns `{ id, parts }` for `complete` later.
async function upload(request, token, file, { filename, contentType = '', complete: finish = true } = {}) {
  const buf = fs.readFileSync(file);
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

/// comp-media's /health: which backend this box evaluates on.
async function mediaHealth(request) {
  const res = await request.get(`${MEDIA}/health`);
  return json(res);
}

module.exports = {
  BASE, MEDIA, SAMPLES, SAMPLE,
  derivePreviewJpeg, jpegMetadata,
  uniqueEmail, signUp, logIn, auth, upload, complete, getPhoto, listPhotos, waitSettled, mediaHealth,
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
