// Photoquest, step one: log in, upload straight to object storage, watch the
// evaluation arrive. No build step — this file is served as it is.
//
// The upload is the part that matters. `POST /api/photos` answers with a plan:
// one presigned PUT URL per part. The browser sends each slice of the file to
// its URL itself (four at a time), keeps the ETag each PUT answers with, and
// hands the list back to `POST /api/photos/{id}/complete`. Reading `ETag` needs
// the bucket's CORS rule to expose it; without that the header reads as null
// and the upload stops with a message saying so, rather than completing an
// upload the store will refuse.

const PARALLEL = 4;
const POLL_MS = 2000;
const $ = (id) => document.getElementById(id);
let token = localStorage.getItem('token') || '';
let polling = null;      // the photo on the detail card, while it settles
let galleryTimer = null; // the gallery, while any photo in it is processing

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

async function api(method, path, body) {
  const headers = token ? { Authorization: 'Bearer ' + token } : {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  const r = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await r.text();
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch (_) { /* not JSON */ }
  if (r.status === 401 && path.startsWith('/api/')) { logout(); }
  return { ok: r.ok, status: r.status, json };
}

function why(res) {
  const j = res.json || {};
  return [j.error, j.detail].filter(Boolean).join(': ') || ('HTTP ' + res.status);
}

// The refusals a person can act on, in words; anything else as the API said it.
function authWhy(res) {
  const e = (res.json || {}).error || '';
  if (e === 'invalid_credentials') return 'wrong email or password';
  if (res.status === 409 || /exist|taken|duplicate/i.test(e)) return 'that email already has an account — log in instead';
  return why(res);
}

// ---- auth -----------------------------------------------------------------

async function register() {
  const email = $('reg-email').value, password = $('reg-password').value;
  const r = await api('POST', '/register', { email, password });
  if (!r.ok) { $('auth-error').textContent = 'registration failed: ' + authWhy(r); return; }
  $('login-email').value = email;
  $('login-password').value = password;
  await login();
}

async function login() {
  const r = await api('POST', '/login', { email: $('login-email').value, password: $('login-password').value });
  if (!r.ok) { $('auth-error').textContent = 'login failed: ' + authWhy(r); return; }
  $('auth-error').textContent = '';
  token = r.json.access_token;
  localStorage.setItem('token', token);
  await show();
}

function logout() {
  if (token) { fetch('/logout', { method: 'POST', headers: { Authorization: 'Bearer ' + token } }); }
  token = '';
  localStorage.removeItem('token');
  clearInterval(polling);
  clearTimeout(galleryTimer);
  // Nothing of this account may stay on the page for whoever logs in next on
  // the same browser: the last photo's card (with its signed URLs) included.
  $('detail').style.display = 'none';
  $('detail').innerHTML = '';
  $('gallery').innerHTML = '';
  $('upload-status').textContent = '';
  $('whoami').textContent = '';
  $('file').value = '';
  $('app').style.display = 'none';
  $('auth').style.display = 'block';
}

async function show() {
  const me = await api('GET', '/me');
  if (!me.ok) { logout(); return; }
  $('whoami').textContent = 'signed in as ' + me.json.subject;
  $('auth').style.display = 'none';
  $('app').style.display = 'block';
  const photos = await gallery();
  // Back after a reload with a photo still being evaluated: watch it again,
  // as if it had just been uploaded.
  const newest = (photos || [])[0];
  if (newest && newest.state === 'processing') {
    $('upload-status').textContent = 'evaluating…';
    watch(newest.id);
  }
}

// ---- upload ---------------------------------------------------------------

// As the browser reported it, "" included — which is what most browsers say
// about an ARW. comp-media decides by the extension when the type says nothing,
// and a type invented here would be one more thing for it to refuse.
function contentType(file) {
  return file.type || '';
}

// One part, by XHR rather than fetch: fetch has no upload progress, and a
// 16 MiB part on a slow link is a long time for a bar to sit still.
function putPart(url, blob, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('PUT', url);
    xhr.upload.onprogress = (e) => onProgress(e.loaded);
    xhr.onload = () => {
      if (xhr.status < 200 || xhr.status >= 300) return reject(new Error('part answered ' + xhr.status));
      const etag = xhr.getResponseHeader('ETag');
      if (!etag) return reject(new Error('no ETag on the part — the bucket CORS rule must expose ETag'));
      resolve(etag);
    };
    xhr.onerror = () => reject(new Error('network error (is the bucket CORS rule set?)'));
    xhr.send(blob);
  });
}

async function putWithRetry(url, blob, onProgress) {
  let last;
  for (let attempt = 1; attempt <= 3; attempt++) {
    try { return await putPart(url, blob, onProgress); } catch (e) { last = e; onProgress(0); }
  }
  throw last;
}

async function upload() {
  const file = $('file').files[0];
  if (!file) { $('upload-status').textContent = 'choose a file first'; return; }
  const status = (s) => { $('upload-status').textContent = s; };
  const bar = $('progress');
  $('upload-btn').disabled = true;
  try {
    status('asking for an upload plan…');
    const created = await api('POST', '/api/photos', { filename: file.name, size: file.size, content_type: contentType(file) });
    if (created.status === 422) {
      // The evaluator's own reason ("not an accepted file type: …") after
      // the one sentence a person needs.
      throw new Error(`"${file.name}" is not accepted — Photoquest takes Sony ARW or JPEG files (${(created.json || {}).detail || 'refused'})`);
    }
    if (!created.ok) throw new Error(why(created));
    const { photo, upload: plan } = created.json;

    const sent = new Array(plan.parts.length).fill(0);
    const etags = [];
    bar.hidden = false; bar.max = file.size; bar.value = 0;
    const tick = () => {
      const total = sent.reduce((a, b) => a + b, 0);
      bar.value = total;
      status(`uploading ${(total / 1048576).toFixed(1)} / ${(file.size / 1048576).toFixed(1)} MiB`);
    };
    let next = 0;
    const worker = async () => {
      while (next < plan.parts.length) {
        const i = next++;
        const part = plan.parts[i];
        const start = (part.number - 1) * plan.part_size;
        const blob = file.slice(start, Math.min(start + plan.part_size, file.size));
        const etag = await putWithRetry(part.url, blob, (n) => { sent[i] = n; tick(); });
        sent[i] = blob.size; tick();
        etags.push({ number: part.number, etag });
      }
    };
    await Promise.all(Array.from({ length: Math.min(PARALLEL, plan.parts.length) }, worker));

    status('finishing the upload…');
    etags.sort((a, b) => a.number - b.number);
    const done = await api('POST', `/api/photos/${photo.id}/complete`, { parts: etags });
    if (!done.ok) throw new Error(why(done));
    status('uploaded — evaluating…');
    $('file').value = '';
    await gallery();
    watch(photo.id);
  } catch (e) {
    status('upload failed: ' + e.message);
  } finally {
    $('upload-btn').disabled = false;
    bar.hidden = true;
  }
}

// ---- viewing --------------------------------------------------------------

function watch(id) {
  clearInterval(polling);
  const once = async () => {
    const r = await api('GET', `/api/photos/${id}`);
    if (!r.ok) { clearInterval(polling); return; }
    detail(r.json);
    if (r.json.state === 'evaluated' || r.json.state === 'failed') {
      clearInterval(polling);
      $('upload-status').textContent = r.json.state === 'evaluated' ? 'evaluated' : 'evaluation failed';
      await gallery();
    }
  };
  once();
  polling = setInterval(once, POLL_MS);
}

function exposure(s) {
  if (!s) return '';
  return s >= 1 ? `${s}s` : `1/${Math.round(1 / s)}s`;
}

function detail(p) {
  const d = $('detail');
  d.style.display = 'block';
  const m = p.metadata || {}, sh = p.sharpness || {}, v = p.vision, b = p.backend || {}, c = p.colour || {};
  const urls = p.urls || {};
  const rows = [];
  const row = (k, val) => { if (val !== undefined && val !== null && val !== '') rows.push(`<dt>${esc(k)}</dt><dd>${val}</dd>`); };
  row('state', `<span class="state-${esc(p.state)}">${esc(p.state)}</span>${p.error ? ' — ' + esc(p.error) : ''}`);
  row('camera', esc(m.camera));
  row('lens', esc(m.lens));
  row('exposure', [exposure(m.exposure_s), m.fnumber && `f/${m.fnumber}`, m.focal_mm && `${m.focal_mm}mm`, m.iso && `ISO ${m.iso}`].filter(Boolean).map(esc).join(' · '));
  row('size', m.width ? esc(`${m.width} × ${m.height}`) : '');
  row('captured', esc(m.captured_at));
  if (sh.focus_ratio != null) {
    row('sharpness', esc(`focus ratio ${Number(sh.focus_ratio).toFixed(1)} (peak ${Number(sh.peak).toFixed(0)} over a floor of ${Number(sh.floor).toFixed(0)})`));
  }
  (sh.subjects || []).forEach((s, i) => row(`face ${i + 1}`, esc(`sharpness ${Number(s.sharpness).toFixed(1)}`)));
  if (v) {
    row('labels', esc((v.labels || []).slice(0, 6).map((l) => `${l.id} ${Math.round(l.confidence * 100)}%`).join(', ')));
    if (v.aesthetics) row('aesthetics', esc(`${Number(v.aesthetics.overall).toFixed(2)}${v.aesthetics.utility ? ' (utility shot)' : ''}`));
  }
  if (c.mean_luma != null) row('exposure clip', esc(`${c.clipped_shadows_pct}% shadows, ${c.clipped_highlights_pct}% highlights`));
  if (b.sharpness) row('evaluated by', esc(`${b.sharpness} sharpness, ${b.develop} develop, ${b.vision ? 'Vision' : 'no Vision'}`));
  const img = urls.thumb ? `<img src="${esc(urls.thumb)}" alt="">` : '';
  const share = urls.share ? `<p><a href="${esc(urls.share)}" target="_blank" rel="noopener">Open the share copy</a> <span class="muted">(link works for an hour)</span></p>` : '';
  d.innerHTML = `<h3>${esc(p.filename)}</h3>${img}${share}<dl>${rows.join('')}</dl>`;
}

async function gallery() {
  clearTimeout(galleryTimer);
  const r = await api('GET', '/api/photos');
  if (!r.ok) return null;
  // A photo still being evaluated: look again shortly, so its tile turns
  // `evaluated` without a click — after a reload too.
  if (r.json.photos.some((p) => p.state === 'processing')) {
    galleryTimer = setTimeout(gallery, POLL_MS);
  }
  $('gallery').innerHTML = r.json.photos.map((p) => `
    <div class="tile" data-id="${esc(p.id)}">
      ${p.thumb_url ? `<img src="${esc(p.thumb_url)}" alt="">` : `<div class="ph">${esc(p.state)}</div>`}
      <div class="name">${esc(p.filename)}</div>
      <div class="muted state-${esc(p.state)}">${esc(p.state)}</div>
    </div>`).join('') || '<p class="muted">Nothing yet.</p>';
  return r.json.photos;
}

$('login-btn').onclick = login;
$('register-btn').onclick = register;
$('logout-btn').onclick = logout;
$('upload-btn').onclick = upload;
$('gallery').onclick = (e) => {
  const tile = e.target.closest('.tile');
  if (tile) watch(tile.dataset.id);
};
if (token) show();
