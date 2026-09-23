// Photoquest: log in, upload straight to object storage, watch the evaluation
// arrive — and the game on top of it: journeys of quests with XP and levels,
// timed competitions, and the curator's and admin's tools. No build step — this
// file is served as it is.
//
// The role-gated tabs (Curator, Admin) are a convenience: the API decides, and a
// 403 from it is shown as a message, never trusted away. Roles are re-read from
// `/me` on every tab switch and every few seconds, so a grant shows up without
// logging in again.
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
let meTimer = null;      // re-reads /me (roles) while signed in
let me = null;           // { subject, roles }
let myPhotos = [];       // the last GET /api/photos
let currentView = 'photos';

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

// The refusals of the API a person can act on, in words.
const ERROR_WORDS = {
  suspended: 'your account is suspended',
  forbidden_role: 'you do not have the role this needs',
  forbidden: 'that is not yours to change',
  not_found: 'not found',
  not_evaluated: 'that photo has not finished evaluating',
  photo_hidden: 'that photo was hidden by a moderator',
  quest_locked: 'this quest is locked — pass the quest before it first',
  quest_not_started: 'this quest has not started yet',
  quest_ended: 'this quest has ended',
  quest_not_published: 'this quest is no longer running',
  quest_published: 'a published quest keeps its requirements — archive it and make a new one',
  journey_archived: 'the journey is archived',
  competition_published: 'a published competition only changes its title and brief',
  competition_archived: 'the competition is archived',
  entry_limit: 'you have entered as many photos as this competition allows',
  already_entered: 'that photo (or the same file) is already entered',
  own_entry: 'you cannot vote on your own entry',
  voting_closed: 'voting has closed',
  judging_closed: 'judging has closed',
  results_pending: 'the results are not out yet',
  already_reported: 'you have already reported this photo',
  own_photo: 'that is your own photo',
  report_closed: 'that report is already closed',
  last_word: 'you cannot revoke your own admin role',
  store_unavailable: 'the store is unavailable — try again',
};

function why(res) {
  const j = res.json || {};
  if (res.status === 403 && j.error === 'forbidden_role') return ERROR_WORDS.forbidden_role;
  const words = ERROR_WORDS[j.error];
  if (words) return j.detail ? `${words} (${j.detail})` : words;
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
  clearInterval(meTimer);
  me = null;
  myPhotos = [];
  // Nothing of this account may stay on the page for whoever logs in next on
  // the same browser: the last photo's card (with its signed URLs) included.
  $('detail').style.display = 'none';
  $('detail').innerHTML = '';
  $('gallery').innerHTML = '';
  $('upload-status').textContent = '';
  $('whoami').textContent = '';
  for (const id of ['xp-summary', 'journeys', 'journey-detail', 'quest-detail', 'competitions',
    'competition-detail', 'progress-view', 'curator-journeys', 'curator-competitions', 'reports', 'users',
    'curator-msg', 'admin-msg']) {
    $(id).innerHTML = '';
  }
  showView('photos');
  $('file').value = '';
  $('app').style.display = 'none';
  $('auth').style.display = 'block';
}

async function show() {
  if (!(await refreshMe())) { logout(); return; }
  $('auth').style.display = 'none';
  $('app').style.display = 'block';
  clearInterval(meTimer);
  meTimer = setInterval(refreshMe, 5000);
  refreshHeader();
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
  const mod = p.moderation && p.moderation.hidden
    ? `<div class="warn" data-testid="moderation-notice"><strong>Hidden by a moderator</strong>${p.moderation.reason ? ': ' + esc(p.moderation.reason) : ''}${p.moderation.at ? ` <span class="muted">(${esc(fmtTime(p.moderation.at))})</span>` : ''}. Other photographers no longer see it, and it cannot be submitted to a quest or entered in a competition.</div>`
    : '';
  const share = urls.share ? `<p><a href="${esc(urls.share)}" target="_blank" rel="noopener">Open the share copy</a> <span class="muted">(link works for an hour)</span></p>` : '';
  d.innerHTML = `<h3>${esc(p.filename)}</h3>${mod}${img}${share}<dl>${rows.join('')}</dl>`;
}

async function gallery() {
  clearTimeout(galleryTimer);
  const r = await api('GET', '/api/photos');
  if (!r.ok) return null;
  myPhotos = r.json.photos;
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
      ${p.moderation && p.moderation.hidden ? '<span class="pill hidden">hidden</span>' : ''}
    </div>`).join('') || '<p class="muted">Nothing yet.</p>';
  return r.json.photos;
}


// ---- the game: shared bits ---------------------------------------------------

/** Unix seconds as local `YYYY-MM-DD HH:MM` — the one format every date on this page uses. */
function fmtTime(secs) {
  if (secs === null || secs === undefined || secs === '') return '';
  const d = new Date(secs * 1000);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** For `<input type="datetime-local">`, and back to unix seconds (null when empty). */
function toLocalInput(secs) {
  return secs === null || secs === undefined ? '' : fmtTime(secs).replace(' ', 'T');
}
function fromLocalInput(v) {
  if (!v) return null;
  const t = new Date(v).getTime();
  return Number.isFinite(t) ? Math.floor(t / 1000) : null;
}

const nowSecs = () => Math.floor(Date.now() / 1000);
const ordinal = (n) => n + ({ 1: 'st', 2: 'nd', 3: 'rd' }[n % 100 > 10 && n % 100 < 14 ? 0 : n % 10] || 'th');
const hasRole = (r) => !!me && (me.roles || []).includes(r);
const q1 = (sel, root = document) => root.querySelector(sel);

/** Re-read `/me`: who, and which tabs a role unlocks. False when not signed in. */
async function refreshMe() {
  if (!token) return false;
  const r = await api('GET', '/me');
  if (!r.ok) return false;
  me = r.json;
  const extra = (me.roles || []).filter((x) => x !== 'photographer');
  $('whoami').textContent = 'signed in as ' + me.subject + (extra.length ? ` (${extra.join(', ')})` : '');
  q1('[data-testid=nav-curator]').hidden = !hasRole('curator');
  q1('[data-testid=nav-admin]').hidden = !hasRole('admin');
  if ((currentView === 'curator' && !hasRole('curator')) || (currentView === 'admin' && !hasRole('admin'))) {
    showView('photos');
  }
  return true;
}

/** The header: total XP and my level in every journey I have XP in. */
async function refreshHeader() {
  const r = await api('GET', '/api/me/progress');
  if (!r.ok) { $('xp-summary').innerHTML = ''; return; }
  const js = r.json.journeys.filter((j) => j.xp > 0);
  const badges = r.json.badges.length;
  $('xp-summary').innerHTML = `<strong data-testid="header-total-xp">${r.json.total_xp} XP</strong>`
    + js.map((j) => ` · <span data-testid="header-level" data-journey="${esc(j.journey)}">${esc(j.title)}: level ${j.level}</span>`).join('')
    + (badges ? ` · <span data-testid="header-badges">${badges} badge${badges === 1 ? '' : 's'}</span>` : '');
}

function showView(name) {
  currentView = name;
  for (const b of document.querySelectorAll('#nav button')) b.classList.toggle('active', b.dataset.view === name);
  for (const s of document.querySelectorAll('.view')) s.classList.toggle('active', s.id === 'view-' + name);
}

async function openView(name) {
  showView(name);
  refreshMe();
  if (name === 'photos') await gallery();
  if (name === 'journeys') { $('journey-detail').innerHTML = ''; $('quest-detail').innerHTML = ''; await renderJourneys(); }
  if (name === 'competitions') { $('competition-detail').innerHTML = ''; await renderCompetitions(); }
  if (name === 'progress') await renderProgress();
  if (name === 'curator') await renderCurator();
  if (name === 'admin') { await renderReports(); await renderUsers(); }
}

/** My evaluated photos that may still be submitted or entered (not hidden). */
async function usablePhotos() {
  const r = await api('GET', '/api/photos');
  if (!r.ok) return [];
  myPhotos = r.json.photos;
  return myPhotos.filter((p) => p.state === 'evaluated' && !(p.moderation && p.moderation.hidden));
}

function photoSelect(testid, photos) {
  if (!photos.length) return '<p class="muted">You have no evaluated photos yet — upload one under “My photos”.</p>';
  return `<select data-testid="${testid}">${photos.map((p) => `<option value="${esc(p.id)}">${esc(p.filename)} — ${esc(fmtTime(p.created_at))}</option>`).join('')}</select>`;
}

/** A requirements object (CONTRACT.md "Requirements") in plain words, one line each. */
function describeRequirements(req, startsAt, what) {
  req = req || {};
  const out = [];
  const s = req.subject;
  if (s && s.face) out.push('At least one face in the picture');
  else if (s && s.label) {
    out.push(`Shows “${s.label}”` + (s.min_confidence ? ` (Vision at least ${Math.round(s.min_confidence * 100)}% sure)` : ''));
  }
  const sh = req.sharpness || {};
  if (sh.min_focus_ratio != null) out.push(`In focus: focus ratio at least ${sh.min_focus_ratio}`);
  if (sh.min_subject_ratio != null) out.push(`A sharp face: subject sharpness ratio at least ${sh.min_subject_ratio}`);
  if (req.aesthetics_min != null) out.push(`Aesthetics score at least ${req.aesthetics_min}`);
  const e = req.exposure || {};
  if (e.max_fnumber != null) out.push(`Aperture f/${e.max_fnumber} or wider`);
  if (e.max_shutter_s != null) out.push(`Shutter ${exposure(e.max_shutter_s)} or faster`);
  if (e.min_focal_mm != null && e.max_focal_mm != null) out.push(`Focal length ${e.min_focal_mm}–${e.max_focal_mm} mm`);
  else if (e.min_focal_mm != null) out.push(`Focal length ${e.min_focal_mm} mm or longer`);
  else if (e.max_focal_mm != null) out.push(`Focal length ${e.max_focal_mm} mm or shorter`);
  if (e.max_iso != null) out.push(`ISO ${e.max_iso} or lower`);
  if (req.format === 'raw') out.push('A RAW original (Sony ARW)');
  if (req.captured_after_start !== false) out.push(`Taken after the ${what} started${startsAt ? ` (${fmtTime(startsAt)})` : ''}`);
  return out.length ? out : ['Any evaluated photo'];
}

const CHECK_NAMES = {
  subject: 'Subject',
  'sharpness.min_focus_ratio': 'Focus',
  'sharpness.min_subject_ratio': 'Face sharpness',
  aesthetics_min: 'Aesthetics',
  'exposure.max_fnumber': 'Aperture',
  'exposure.max_shutter_s': 'Shutter',
  'exposure.min_focal_mm': 'Focal length (min)',
  'exposure.max_focal_mm': 'Focal length (max)',
  'exposure.max_iso': 'ISO',
  format: 'RAW original',
  captured_after_start: 'Taken after the start',
};

/** One line per check: ✓ passed, ✗ failed, — not looked at (the stage did not run). */
function renderChecks(verdict) {
  return `<ul class="checks">${((verdict || {}).checks || []).map((c) => {
    const [cls, sym] = c.ok === true ? ['yes', '✓'] : c.ok === false ? ['no', '✗'] : ['na', '—'];
    return `<li data-testid="check" data-name="${esc(c.name)}" data-ok="${String(c.ok)}"><span class="sym ${cls}">${sym}</span>${esc(CHECK_NAMES[c.name] || c.name)}: <span class="muted">${esc(c.detail)}</span></li>`;
  }).join('')}</ul>`;
}

function xpLine(sub) {
  if (!sub.pass) return 'No XP — the photo did not pass every check.';
  if (sub.xp_awarded > 0) return `+${sub.xp_awarded} XP`;
  if (sub.xp_reason === 'already_rewarded') return 'Passed, but no XP this time: this file has already earned XP once (the same photo, or the same bytes uploaded again) — a file earns XP only once.';
  if (sub.xp_reason === 'already_passed') return 'Passed, but no XP this time: you were already awarded this quest’s XP.';
  return 'Passed — this quest awards no XP.';
}

// ---- journeys & quests (photographer) -----------------------------------------------

function levelLine(p) {
  return `Level ${p.level} · ${p.xp} XP` + (p.next_level_xp != null ? ` · next level at ${p.next_level_xp} XP` : ' · top level');
}

function badgeText(badge, earned) {
  if (earned) return ` · <span data-testid="journey-badge">Badge earned: ${esc(earned.name)}</span>`;
  if (badge && badge.name) return ` · finishing it earns the badge “${esc(badge.name)}”`;
  return '';
}

async function renderJourneys() {
  const r = await api('GET', '/api/journeys');
  if (!r.ok) { $('journeys').innerHTML = `<p class="err">${esc(why(r))}</p>`; return; }
  $('journeys').innerHTML = '<h3>Journeys</h3>' + (r.json.journeys.slice().reverse().map((j) => {
    const p = j.progress;
    return `<div class="card click" data-testid="journey-card" data-id="${esc(j.id)}">
      <div class="row"><strong data-testid="journey-title">${esc(j.title)}</strong><span data-testid="journey-level">${esc(levelLine(p))}</span></div>
      ${j.description ? `<div class="muted">${esc(j.description)}</div>` : ''}
      ${p.next_level_xp != null ? `<progress value="${p.xp}" max="${p.next_level_xp}"></progress>` : ''}
      <div class="muted">${j.passed_count} of ${j.quest_count} quests passed${badgeText(j.badge, p.badge)}</div>
    </div>`;
  }).join('') || '<p class="muted">No journeys are open yet.</p>');
}

function questWindow(q) {
  if (q.window === 'upcoming') return `starts ${fmtTime(q.starts_at)}`;
  if (q.window === 'ended') return `ended ${fmtTime(q.ends_at)}`;
  return q.ends_at ? `open until ${fmtTime(q.ends_at)}` : 'no deadline';
}

async function openJourney(id) {
  $('quest-detail').innerHTML = '';
  $('journeys').innerHTML = '';
  await renderJourneyDetail(id);
}

async function renderJourneyDetail(id) {
  const r = await api('GET', `/api/journeys/${id}`);
  if (!r.ok) { $('journey-detail').innerHTML = `<p class="err">${esc(why(r))}</p>`; return; }
  const j = r.json, p = j.progress;
  const rows = j.quests.map((q, i) => {
    const prev = j.quests[i - 1];
    return `<div class="card ${q.state !== 'locked' ? 'click' : ''}" data-testid="quest-row" data-id="${esc(q.id)}" data-state="${esc(q.state)}">
      <div class="row"><strong data-testid="quest-title">${esc(q.title)}</strong><span class="pill ${esc(q.state)}" data-testid="quest-state">${esc(q.state)}</span><span>${q.xp} XP</span><span class="muted" data-testid="quest-window">${esc(questWindow(q))}</span></div>
      ${q.state === 'locked' && prev ? `<div class="muted">Locked — pass “${esc(prev.title)}” first</div>` : ''}
    </div>`;
  }).join('');
  $('journey-detail').innerHTML = `<div class="card" data-testid="journey-detail" data-id="${esc(j.id)}">
    <div class="row"><h3 style="margin:0">${esc(j.title)}</h3><button data-testid="journeys-back">All journeys</button></div>
    ${j.description ? `<p>${esc(j.description)}</p>` : ''}
    <p><strong data-testid="journey-level">${esc(levelLine(p))}</strong>${badgeText(j.badge, p.badge)}</p>
    ${p.next_level_xp != null ? `<progress value="${p.xp}" max="${p.next_level_xp}"></progress>` : ''}
    <p class="muted">Levels: ${esc((j.levels || []).map((l) => `${l.level} at ${l.xp} XP`).join(', '))}</p>
    <h4>Quests, in order</h4>
    ${rows || '<p class="muted">No quests yet.</p>'}
  </div>`;
}

async function openQuest(id) {
  const [qr, sr, photos] = await Promise.all([
    api('GET', `/api/quests/${id}`), api('GET', `/api/quests/${id}/submissions`), usablePhotos(),
  ]);
  if (!qr.ok) { $('quest-detail').innerHTML = `<p class="err">${esc(why(qr))}</p>`; return; }
  const q = qr.json;
  const names = new Map(myPhotos.map((p) => [p.id, p.filename]));
  const subs = sr.ok ? sr.json.submissions : [];
  let action;
  if (q.state === 'locked') action = '<p class="muted">Locked — pass the quest before it first.</p>';
  else if (q.window === 'upcoming') action = `<p class="muted">Not started yet — it starts ${esc(fmtTime(q.starts_at))}.</p>`;
  else if (q.window === 'ended') action = `<p class="muted">This quest ended ${esc(fmtTime(q.ends_at))}.</p>`;
  else action = `<h4>Submit a photo</h4><div class="row">${photoSelect('submit-photo-select', photos)}${photos.length ? '<button data-testid="submit-photo-btn">Submit a photo</button>' : ''}</div>`;
  $('quest-detail').innerHTML = `<div class="card" data-testid="quest-detail" data-id="${esc(q.id)}" data-journey-title="${esc(q.journey_title)}">
    <h3>${esc(q.title)}</h3>
    <div class="muted">${esc(q.journey_title)} · <span data-testid="quest-xp">${q.xp} XP</span> · <span data-testid="quest-window">${esc(questWindow(q))}</span> · <span class="pill ${esc(q.state)}">${esc(q.state)}</span></div>
    ${q.description ? `<p>${esc(q.description)}</p>` : ''}
    <h4>What it asks for</h4>
    <ul data-testid="quest-requirements">${describeRequirements(q.requirements, q.starts_at, 'quest').map((x) => `<li>${esc(x)}</li>`).join('')}</ul>
    ${action}
    <div data-testid="verdict"></div>
    ${subs.length ? `<h4>My submissions</h4>${subs.slice(0, 10).map((s) => `<div class="muted" data-testid="submission">${esc(fmtTime(s.at))} — ${esc(names.get(s.photo) || s.photo)} — ${s.pass ? 'passed' : 'not passed'}${s.xp_awarded ? `, +${s.xp_awarded} XP` : ''}</div>`).join('')}` : ''}
  </div>`;
}

function verdictHtml(sub, journeyTitle) {
  const lu = sub.level_up;
  return `<div class="${sub.pass ? 'notice' : 'warn'}">
    <strong data-testid="verdict-result">${sub.pass ? 'Passed' : 'Not passed'}</strong>
    ${renderChecks(sub.verdict)}
    <div data-testid="xp-line">${esc(xpLine(sub))}</div>
    ${lu ? `<div data-testid="level-up"><strong>Level up!</strong> ${esc(journeyTitle)}: level ${lu.from} → level ${lu.to}</div>` : ''}
    ${sub.badge ? `<div data-testid="badge-notice"><strong>Badge earned:</strong> ${esc(sub.badge.name)}</div>` : ''}
  </div>`;
}

async function submitToQuest() {
  const detail = q1('[data-testid=quest-detail]');
  const sel = q1('[data-testid=submit-photo-select]', detail);
  const out = q1('[data-testid=verdict]', detail);
  const btn = q1('[data-testid=submit-photo-btn]', detail);
  btn.disabled = true;
  const r = await api('POST', `/api/quests/${detail.dataset.id}/submissions`, { photo_id: sel.value });
  btn.disabled = false;
  if (!r.ok) { out.innerHTML = `<p class="err" data-testid="verdict-error">Not submitted: ${esc(why(r))}</p>`; return; }
  out.innerHTML = verdictHtml(r.json, detail.dataset.journeyTitle);
  refreshHeader();
  const jd = q1('[data-testid=journey-detail]');
  if (jd) await renderJourneyDetail(jd.dataset.id);
}

// ---- competitions (photographer) ---------------------------------------------------

const PHASE_WORDS = { upcoming: 'upcoming', open: 'open for entries', voting: 'voting', judging: 'judging', finished: 'ended', archived: 'archived' };

function phaseLine(c) {
  switch (c.phase) {
    case 'upcoming': return `entries open ${fmtTime(c.opens_at)}`;
    case 'open': return `entries close ${fmtTime(c.closes_at)}`;
    case 'voting': return `entries closed · voting until ${fmtTime(c.voting_closes_at)}`;
    case 'judging': return `judging · results ${fmtTime(c.judging_closes_at)}`;
    default: return `ended · results since ${fmtTime(c.judging_closes_at)}`;
  }
}

async function renderCompetitions() {
  const r = await api('GET', '/api/competitions');
  if (!r.ok) { $('competitions').innerHTML = `<p class="err">${esc(why(r))}</p>`; return; }
  $('competitions').innerHTML = '<h3>Competitions</h3>' + (r.json.competitions.map((c) => `
    <div class="card click" data-testid="competition-card" data-id="${esc(c.id)}">
      <div class="row"><strong data-testid="competition-title">${esc(c.title)}</strong><span class="pill" data-testid="competition-phase">${esc(PHASE_WORDS[c.phase] || c.phase)}</span></div>
      <div class="muted">${esc(phaseLine(c))}</div>
    </div>`).join('') || '<p class="muted">No competitions yet.</p>');
}

function partText(name, p) {
  const w = `×${Math.round((p.weight || 0) * 100)}%`;
  if (name === 'auto') return `automatic ${Number(p.value).toFixed(2)} ${w}${p.flags && p.flags.length ? ' (' + p.flags.join('; ') + ')' : ''}`;
  if (p.no_inputs) return `${name} ${w}: none yet`;
  if (name === 'votes') return `votes ${Number(p.value).toFixed(2)} ${w} (${p.count} vote${p.count === 1 ? '' : 's'}, mean ${Number(p.mean).toFixed(1)}★)`;
  return `judges ${Number(p.value).toFixed(2)} ${w} (${p.count} score${p.count === 1 ? '' : 's'}, mean ${Number(p.mean).toFixed(1)}/10)`;
}

function leaderboardHtml(lb, c) {
  if (!lb.entries.length) return '<p class="muted" data-testid="leaderboard-empty">No entries yet.</p>';
  const canVote = c.phase === 'open' || c.phase === 'voting';
  return `<table data-testid="leaderboard"><tr><th>#</th><th></th><th>photographer</th><th>score</th><th>parts</th><th></th></tr>
    ${lb.entries.map((e) => `<tr data-testid="lb-row" data-entry="${esc(e.entry)}" data-photo="${esc(e.photo)}">
      <td data-testid="lb-rank">${e.rank}</td>
      <td>${e.thumb_url ? `<img class="thumb" src="${esc(e.thumb_url)}" alt="">` : '<div class="thumb"></div>'}</td>
      <td data-testid="lb-entrant">${esc(e.entrant.display_name)}${e.mine ? ' <span class="muted">(you)</span>' : ''}</td>
      <td data-testid="lb-score">${Number(e.score).toFixed(2)}</td>
      <td class="muted">${['auto', 'votes', 'judges'].map((k) => esc(partText(k, e.parts[k]))).join('<br>')}</td>
      <td>${e.mine ? '<span class="muted">your entry</span>' : `${canVote ? `<span class="stars" data-testid="stars">${[1, 2, 3, 4, 5].map((n) => `<button data-testid="star-${n}" data-stars="${n}" class="${(e.my_vote || 0) >= n ? 'on' : ''}" title="${n} of 5">★</button>`).join('')}</span><br>` : ''}<button data-testid="report-btn">Report</button><div data-testid="report-form"></div>`}</td>
    </tr>`).join('')}</table>`;
}

function resultsHtml(r) {
  const winners = new Set(r.winners.map((w) => w.entry));
  return `<h4>Results</h4><div data-testid="results">
    ${r.winners.length ? `<ul>${r.winners.map((w) => `<li data-testid="winner"><strong>${ordinal(w.place)} place</strong>: ${esc(w.entrant.display_name)}${w.mine ? ' (you)' : ''} — ${w.xp} XP${w.credited ? '' : ' (being credited)'}</li>`).join('')}</ul>` : '<p class="muted">No prizes in this one.</p>'}
    <table><tr><th>#</th><th>photographer</th><th>score</th></tr>
    ${r.ranking.map((x) => `<tr data-testid="result-row" data-entry="${esc(x.entry)}"><td>${x.rank}</td><td>${esc(x.entrant.display_name)}${x.mine ? ' (you)' : ''}${winners.has(x.entry) ? ' — <strong>winner</strong>' : ''}</td><td>${Number(x.score).toFixed(2)}</td></tr>`).join('')}
    </table></div>`;
}

let currentCompetition = null;

async function openCompetition(id, message) {
  const [dr, lr, photos] = await Promise.all([
    api('GET', `/api/competitions/${id}`), api('GET', `/api/competitions/${id}/leaderboard`), usablePhotos(),
  ]);
  if (!dr.ok) { $('competition-detail').innerHTML = `<p class="err">${esc(why(dr))}</p>`; return; }
  const c = dr.json;
  currentCompetition = c;
  let results = null;
  if (c.results_available) {
    const rr = await api('GET', `/api/competitions/${id}/results`);
    if (rr.ok) results = rr.json;
  }
  $('competitions').innerHTML = '';
  const w = c.weights || {};
  const pct = (x) => Math.round((x || 0) * 100) + '%';
  const prizes = (c.prizes_xp || []).map((xp, i) => `${ordinal(i + 1)} ${xp} XP`).join(', ') || 'none';
  let enter;
  if (c.phase === 'open') {
    enter = `<h4>Enter a photo</h4><p class="muted">You have entered ${c.my_entries.length} of ${c.max_entries_per_user}.</p>
      <div class="row">${photoSelect('enter-photo-select', photos)}${photos.length ? '<button data-testid="enter-btn">Enter this photo</button>' : ''}</div>`;
  } else if (c.phase === 'upcoming') {
    enter = `<p class="muted" data-testid="enter-closed">Entries open ${esc(fmtTime(c.opens_at))}.</p>`;
  } else {
    enter = `<p class="muted" data-testid="enter-closed">Entries closed at ${esc(fmtTime(c.closes_at))}.</p>`;
  }
  $('competition-detail').innerHTML = `<div class="card" data-testid="competition-detail" data-id="${esc(c.id)}">
    <div class="row"><h3 style="margin:0">${esc(c.title)}</h3><span class="pill" data-testid="competition-phase">${esc(PHASE_WORDS[c.phase] || c.phase)}</span><button data-testid="competitions-back">All competitions</button></div>
    ${c.brief ? `<p data-testid="competition-brief">${esc(c.brief)}</p>` : ''}
    <dl>
      <dt>entries</dt><dd>${esc(fmtTime(c.opens_at))} – ${esc(fmtTime(c.closes_at))}</dd>
      <dt>voting until</dt><dd>${esc(fmtTime(c.voting_closes_at))}</dd>
      <dt>results</dt><dd>${esc(fmtTime(c.judging_closes_at))}</dd>
      <dt>scoring</dt><dd>automatic ${pct(w.auto)} · votes ${pct(w.votes)} · judges ${pct(w.judges)}</dd>
      <dt>prizes</dt><dd>${esc(prizes)}${c.journey ? ' (counts toward a journey’s levels)' : ''}</dd>
      <dt>entries each</dt><dd>${c.max_entries_per_user}</dd>
    </dl>
    <h4>What it asks for</h4>
    <ul data-testid="competition-requirements">${describeRequirements(c.requirements, c.opens_at, 'competition').map((x) => `<li>${esc(x)}</li>`).join('')}</ul>
    ${enter}
    <div data-testid="competition-msg">${message || ''}</div>
    ${results ? resultsHtml(results) : ''}
    ${results ? '' : `<h4>Leaderboard</h4>${lr.ok ? leaderboardHtml(lr.json, c) : `<p class="err">${esc(why(lr))}</p>`}`}
  </div>`;
}

async function enterCompetition() {
  const c = currentCompetition;
  const out = q1('[data-testid=competition-msg]');
  const sel = q1('[data-testid=enter-photo-select]');
  const r = await api('POST', `/api/competitions/${c.id}/entries`, { photo_id: sel.value });
  if (r.ok) { await openCompetition(c.id, '<p class="ok" data-testid="enter-ok">Entered — good luck.</p>'); return; }
  const j = r.json || {};
  if (r.status === 422 && j.error === 'ineligible') {
    out.innerHTML = `<div class="warn"><strong data-testid="enter-error">Not eligible</strong> — the photo does not meet the requirements:${renderChecks(j.verdict)}</div>`;
  } else if (j.error === 'competition_closed') {
    const when = j.detail === 'not open yet' ? `Entries open ${fmtTime(c.opens_at)}` : `Entries closed at ${fmtTime(c.closes_at)}`;
    out.innerHTML = `<p class="err" data-testid="enter-error">Not entered: ${esc(when)}.</p>`;
  } else {
    out.innerHTML = `<p class="err" data-testid="enter-error">Not entered: ${esc(why(r))}</p>`;
  }
}

async function vote(btn) {
  const c = currentCompetition;
  const row = btn.closest('[data-testid=lb-row]');
  const r = await api('PUT', `/api/competitions/${c.id}/entries/${row.dataset.entry}/vote`, { stars: Number(btn.dataset.stars) });
  const msg = r.ok ? `<p class="ok" data-testid="vote-ok">Vote saved: ${btn.dataset.stars} of 5.</p>` : `<p class="err" data-testid="vote-error">Vote not saved: ${esc(why(r))}</p>`;
  await openCompetition(c.id, msg);
}

function showReportForm(btn) {
  const form = q1('[data-testid=report-form]', btn.closest('td'));
  form.innerHTML = `<select data-testid="report-reason">
      <option value="inappropriate">inappropriate</option><option value="stolen">stolen (not their photo)</option>
      <option value="spam">spam</option><option value="other">other</option></select>
    <input data-testid="report-note" placeholder="note (optional)">
    <button data-testid="report-submit">Send report</button><div data-testid="report-msg"></div>`;
}

async function sendReport(btn) {
  const td = btn.closest('td');
  const row = btn.closest('[data-testid=lb-row]');
  const note = q1('[data-testid=report-note]', td).value.trim();
  const r = await api('POST', `/api/photos/${row.dataset.photo}/reports`, {
    reason: q1('[data-testid=report-reason]', td).value, note: note || null,
  });
  q1('[data-testid=report-msg]', td).innerHTML = r.ok
    ? '<span class="ok">Reported — an admin will look at it.</span>'
    : `<span class="err">Not reported: ${esc(why(r))}</span>`;
}

// ---- progress --------------------------------------------------------------------

const sourceTitles = new Map();
async function sourceTitle(row) {
  const key = row.source + ':' + row.source_id;
  if (sourceTitles.has(key)) return sourceTitles.get(key);
  let t = row.source;
  if (row.source === 'quest') {
    const r = await api('GET', `/api/quests/${row.source_id}`);
    t = r.ok ? `Quest “${r.json.title}”` : 'A quest';
  } else if (row.source === 'competition') {
    const [cid, place] = String(row.source_id).split('#');
    const r = await api('GET', `/api/competitions/${cid}`);
    t = `${r.ok ? `Competition “${r.json.title}”` : 'A competition'}, ${ordinal(Number(place))} place`;
  }
  sourceTitles.set(key, t);
  return t;
}

async function renderProgress() {
  const [r] = await Promise.all([api('GET', '/api/me/progress'), usablePhotos()]);
  if (!r.ok) { $('progress-view').innerHTML = `<p class="err">${esc(why(r))}</p>`; return; }
  const pr = r.json;
  const names = new Map(myPhotos.map((p) => [p.id, p.filename]));
  const titles = new Map(pr.journeys.map((j) => [j.journey, j.title]));
  const mine = pr.journeys.filter((j) => j.xp > 0 || j.badge);
  const whats = await Promise.all(pr.ledger.map(sourceTitle));
  $('progress-view').innerHTML = `<div class="card">
    <h3>Progress</h3>
    <p>Total: <strong data-testid="total-xp">${pr.total_xp} XP</strong></p>
    <h4>Journeys</h4>
    ${mine.length ? `<table><tr><th>journey</th><th>level</th><th>XP</th><th>next level</th></tr>${mine.map((j) => `<tr data-testid="progress-journey" data-journey="${esc(j.journey)}"><td>${esc(j.title)}</td><td>${j.level}</td><td>${j.xp}</td><td>${j.next_level_xp != null ? j.next_level_xp + ' XP' : 'top level'}</td></tr>`).join('')}</table>` : '<p class="muted">No XP yet — pass a quest to start.</p>'}
    <h4>Badges</h4>
    ${pr.badges.length ? `<ul>${pr.badges.map((b) => `<li data-testid="badge"><strong>${esc(b.name)}</strong> — ${esc(titles.get(b.journey) || 'a journey')} <span class="muted">(${esc(fmtTime(b.at))})</span></li>`).join('')}</ul>` : '<p class="muted">None yet — finish a journey to earn its badge.</p>'}
    <h4>XP history</h4>
    ${pr.ledger.length ? `<table><tr><th>when</th><th>for</th><th>photo</th><th>XP</th></tr>${pr.ledger.map((l, i) => `<tr data-testid="ledger-row"><td>${esc(fmtTime(l.at))}</td><td data-testid="ledger-what">${esc(whats[i])}</td><td>${esc(names.get(l.photo) || l.photo)}</td><td data-testid="ledger-xp">+${l.xp}</td></tr>`).join('')}</table>` : '<p class="muted">Nothing yet.</p>'}
  </div>`;
}

// ---- curator ---------------------------------------------------------------------

let curatorPane = 'journeys';
let curJourneyId = null;

async function renderCurator() {
  $('curator-msg').textContent = '';
  $('curator-journeys').hidden = curatorPane !== 'journeys';
  $('curator-competitions').hidden = curatorPane !== 'competitions';
  if (curatorPane === 'journeys') await renderCuratorJourneys();
  else await renderCuratorCompetitions();
}

/** A 403 from a curator route: say so, and stop pretending the tab is usable. */
function curatorRefused(r) {
  $('curator-msg').textContent = r.status === 403 ? `Refused: ${why(r)}.` : why(r);
}

function reqForm(p, r) {
  r = r || {};
  const s = r.subject || {}, sh = r.sharpness || {}, e = r.exposure || {};
  const num = (id, v, label, step = 'any') => `<label>${label} <input type="number" step="${step}" min="0" id="${p}-${id}" data-testid="${p}-${id}" value="${v ?? ''}"></label>`;
  return `<div class="row"><label>Subject <select id="${p}-subject" data-testid="${p}-subject">
        <option value="none">anything</option><option value="label">a Vision label</option><option value="face">at least one face</option></select></label>
      <label>label <input id="${p}-label" data-testid="${p}-label" value="${esc(s.label || '')}" placeholder="e.g. grass"></label>
      ${num('minconf', s.min_confidence, 'min confidence (0–1)')}</div>
    <div class="row">${num('focus', sh.min_focus_ratio, 'min focus ratio')}${num('subjratio', sh.min_subject_ratio, 'min face sharpness ratio')}${num('aesthetics', r.aesthetics_min, 'min aesthetics (0–1)')}</div>
    <div class="row">${num('fnumber', e.max_fnumber, 'max f-number')}${num('shutter', e.max_shutter_s, 'max shutter (s)')}${num('minfocal', e.min_focal_mm, 'min focal (mm)')}${num('maxfocal', e.max_focal_mm, 'max focal (mm)')}${num('iso', e.max_iso, 'max ISO', '1')}</div>
    <div class="row"><label><input type="checkbox" id="${p}-raw" data-testid="${p}-raw" ${r.format === 'raw' ? 'checked' : ''}> RAW original (ARW) only</label>
      <label><input type="checkbox" id="${p}-after" data-testid="${p}-after" ${r.captured_after_start === false ? '' : 'checked'}> taken after the start</label></div>`;
}

function initReqForm(p, r, locked) {
  const s = (r || {}).subject || {};
  $(p + '-subject').value = s.face ? 'face' : s.label ? 'label' : 'none';
  if (locked) for (const el of $(p + '-req').querySelectorAll('input, select')) el.disabled = true;
}

function readReqForm(p) {
  const v = (id) => { const x = $(p + '-' + id).value.trim(); return x === '' ? null : Number(x); };
  const r = {};
  const kind = $(p + '-subject').value;
  if (kind === 'face') r.subject = { face: true };
  if (kind === 'label') {
    r.subject = { label: $(p + '-label').value.trim() };
    if (v('minconf') != null) r.subject.min_confidence = v('minconf');
  }
  const sh = {};
  if (v('focus') != null) sh.min_focus_ratio = v('focus');
  if (v('subjratio') != null) sh.min_subject_ratio = v('subjratio');
  if (Object.keys(sh).length) r.sharpness = sh;
  if (v('aesthetics') != null) r.aesthetics_min = v('aesthetics');
  const e = {};
  for (const [id, k] of [['fnumber', 'max_fnumber'], ['shutter', 'max_shutter_s'], ['minfocal', 'min_focal_mm'], ['maxfocal', 'max_focal_mm'], ['iso', 'max_iso']]) {
    if (v(id) != null) e[k] = v(id);
  }
  if (Object.keys(e).length) r.exposure = e;
  if ($(p + '-raw').checked) r.format = 'raw';
  r.captured_after_start = $(p + '-after').checked;
  return r;
}

function setMsg(id, ok, text) {
  $(id).innerHTML = `<span class="${ok ? 'ok' : 'err'}">${esc(text)}</span>`;
}

async function renderCuratorJourneys() {
  const r = await api('GET', '/api/curator/journeys');
  if (!r.ok) { curatorRefused(r); $('curator-journeys').innerHTML = ''; return; }
  const list = r.json.journeys.slice().reverse();
  $('curator-journeys').innerHTML = `<div class="card"><div class="row"><h3 style="margin:0">Journeys</h3><button data-testid="cur-new-journey">New journey</button></div>
    <table>${list.map((j) => `<tr data-testid="cur-journey-row" data-id="${esc(j.id)}"><td>${esc(j.title)}</td><td><span class="pill">${esc(j.state)}</span></td><td>${(j.quests || []).length} quests</td><td><button data-testid="cur-edit-journey">Edit</button></td></tr>`).join('') || '<tr><td class="muted">None yet.</td></tr>'}</table></div>
    <div id="cur-journey-editor"></div><div id="cur-quest-editor"></div>`;
  if (curJourneyId) await openJourneyEditor(curJourneyId);
}

function levelsHtml(levels) {
  return levels.map((l, i) => `<div class="row" data-testid="cj-level">Level ${i + 1}: <input type="number" min="0" data-testid="cj-level-xp" value="${l.xp}" ${i === 0 ? 'disabled' : ''}> XP ${i > 0 ? '<button data-testid="cj-remove-level">Remove</button>' : ''}</div>`).join('');
}

function readLevels() {
  return [...document.querySelectorAll('[data-testid=cj-level-xp]')].map((el, i) => ({ level: i + 1, xp: i === 0 ? 0 : Number(el.value) }));
}

async function openJourneyEditor(id) {
  let j = { title: '', description: '', levels: [{ level: 1, xp: 0 }], badge: null, quests: [], quest_docs: [], state: 'draft' };
  if (id) {
    const r = await api('GET', `/api/curator/journeys/${id}`);
    if (!r.ok) { curatorRefused(r); return; }
    j = r.json;
  }
  curJourneyId = id;
  $('cur-quest-editor').innerHTML = '';
  const quests = (j.quest_docs || []).map((q, i) => `<div class="row" data-testid="cj-quest" data-id="${esc(q.id)}">
      ${i + 1}. <strong>${esc(q.title)}</strong> <span class="pill">${esc(q.state)}</span> ${q.xp} XP
      <button data-testid="cj-quest-up" ${i === 0 ? 'disabled' : ''}>↑</button><button data-testid="cj-quest-down" ${i === j.quest_docs.length - 1 ? 'disabled' : ''}>↓</button>
      <button data-testid="cj-edit-quest">Edit</button></div>`).join('');
  $('cur-journey-editor').innerHTML = `<div class="card" data-testid="journey-editor" data-id="${esc(id || '')}">
    <h3>${id ? 'Edit journey' : 'New journey'} ${id ? `<span class="pill" data-testid="cj-state">${esc(j.state)}</span>` : ''}</h3>
    <label>Title <input id="cj-title" data-testid="cj-title" value="${esc(j.title)}"></label>
    <label>Badge for finishing it <input id="cj-badge" data-testid="cj-badge" value="${esc((j.badge && j.badge.name) || '')}" placeholder="none"></label>
    <div>Description</div><textarea id="cj-description" data-testid="cj-description">${esc(j.description)}</textarea>
    <fieldset><legend>Levels — XP needed for each</legend><div id="cj-levels">${levelsHtml(j.levels || [])}</div>
      <button data-testid="cj-add-level">Add level</button></fieldset>
    <div class="row"><button data-testid="cj-save">Save</button>
      ${id ? `<button data-testid="cj-publish">${j.state === 'published' ? 'Published' : 'Publish'}</button><button data-testid="cj-archive">Archive</button>` : ''}</div>
    <div id="cj-msg" data-testid="cj-msg"></div>
    ${id ? `<h4>Quests, in unlock order</h4>${quests || '<p class="muted">None yet.</p>'}<button data-testid="cj-new-quest">New quest in this journey</button>` : ''}
  </div>`;
}

async function saveJourney() {
  const id = q1('[data-testid=journey-editor]').dataset.id;
  const badge = $('cj-badge').value.trim();
  const body = { title: $('cj-title').value, description: $('cj-description').value, levels: readLevels(), badge: badge ? { name: badge } : null };
  const r = id ? await api('PUT', `/api/curator/journeys/${id}`, body) : await api('POST', '/api/curator/journeys', body);
  if (!r.ok) { setMsg('cj-msg', false, 'Not saved: ' + why(r)); return; }
  curJourneyId = r.json.id;
  await renderCuratorJourneys();
  setMsg('cj-msg', true, 'Saved.');
}

async function journeyState(action) {
  const id = curJourneyId;
  const r = await api('POST', `/api/curator/journeys/${id}/${action}`);
  if (!r.ok) { setMsg('cj-msg', false, `Not ${action}ed: ` + why(r)); return; }
  await renderCuratorJourneys();
  setMsg('cj-msg', true, action === 'publish' ? 'Published — photographers can see it.' : 'Archived.');
}

async function moveQuest(btn, delta) {
  const qid = btn.closest('[data-testid=cj-quest]').dataset.id;
  const order = [...document.querySelectorAll('[data-testid=cj-quest]')].map((el) => el.dataset.id);
  const i = order.indexOf(qid), k = i + delta;
  if (k < 0 || k >= order.length) return;
  [order[i], order[k]] = [order[k], order[i]];
  const r = await api('PUT', `/api/curator/journeys/${curJourneyId}`, { quests: order });
  if (!r.ok) { setMsg('cj-msg', false, 'Not reordered: ' + why(r)); return; }
  await openJourneyEditor(curJourneyId);
}

async function openQuestEditor(id) {
  let q = { title: '', description: '', xp: 50, starts_at: nowSecs(), ends_at: null, requirements: {}, state: 'draft' };
  if (id) {
    const r = await api('GET', `/api/curator/quests/${id}`);
    if (!r.ok) { curatorRefused(r); return; }
    q = r.json;
  }
  const locked = q.state !== 'draft';
  $('cur-quest-editor').innerHTML = `<div class="card" data-testid="quest-editor" data-id="${esc(id || '')}">
    <h3>${id ? 'Edit quest' : 'New quest'} ${id ? `<span class="pill" data-testid="cq-state">${esc(q.state)}</span>` : ''}</h3>
    <label>Title <input id="cq-title" data-testid="cq-title" value="${esc(q.title)}"></label>
    <label>XP <input type="number" min="0" id="cq-xp" data-testid="cq-xp" value="${q.xp}"></label>
    <div>Description</div><textarea id="cq-description" data-testid="cq-description">${esc(q.description)}</textarea>
    <div class="row"><label>Starts <input type="datetime-local" id="cq-starts" data-testid="cq-starts" value="${toLocalInput(q.starts_at)}"></label>
      <label>Ends <input type="datetime-local" id="cq-ends" data-testid="cq-ends" value="${toLocalInput(q.ends_at)}"></label><span class="muted">(empty = no deadline)</span></div>
    <fieldset id="cq-req"><legend>Requirements${locked ? ' — fixed once published; archive and make a new quest to change them' : ''}</legend>${reqForm('cq', q.requirements)}</fieldset>
    <div class="row"><button data-testid="cq-save">Save</button>
      ${id ? `<button data-testid="cq-publish">Publish</button><button data-testid="cq-archive">Archive</button>` : ''}</div>
    <div id="cq-msg" data-testid="cq-msg"></div>
  </div>`;
  initReqForm('cq', q.requirements, locked);
}

async function saveQuest() {
  const ed = q1('[data-testid=quest-editor]');
  const id = ed.dataset.id;
  const body = {
    title: $('cq-title').value, description: $('cq-description').value, xp: Number($('cq-xp').value || 0),
    starts_at: fromLocalInput($('cq-starts').value), ends_at: fromLocalInput($('cq-ends').value),
  };
  if (!$('cq-subject').disabled) body.requirements = readReqForm('cq');
  let r;
  if (id) r = await api('PUT', `/api/curator/quests/${id}`, body);
  else r = await api('POST', '/api/curator/quests', { ...body, journey: curJourneyId });
  if (!r.ok) { setMsg('cq-msg', false, 'Not saved: ' + why(r)); return; }
  await openJourneyEditor(curJourneyId);
  await openQuestEditor(r.json.id);
  setMsg('cq-msg', true, 'Saved.');
}

async function questState(action) {
  const id = q1('[data-testid=quest-editor]').dataset.id;
  const r = await api('POST', `/api/curator/quests/${id}/${action}`);
  if (!r.ok) { setMsg('cq-msg', false, `Not ${action}ed: ` + why(r)); return; }
  await openJourneyEditor(curJourneyId);
  await openQuestEditor(id);
  setMsg('cq-msg', true, action === 'publish' ? 'Published.' : 'Archived.');
}

async function renderCuratorCompetitions() {
  const r = await api('GET', '/api/curator/competitions');
  if (!r.ok) { curatorRefused(r); $('curator-competitions').innerHTML = ''; return; }
  $('curator-competitions').innerHTML = `<div class="card"><div class="row"><h3 style="margin:0">Competitions</h3><button data-testid="cc-new">New competition</button></div>
    <table>${r.json.competitions.map((c) => `<tr data-testid="cur-comp-row" data-id="${esc(c.id)}"><td>${esc(c.title)}</td><td><span class="pill">${esc(c.state)}</span></td>
      <td class="muted">entries until ${esc(fmtTime(c.closes_at))}, results ${esc(fmtTime(c.judging_closes_at))}</td>
      <td><button data-testid="cc-edit">Edit</button>${c.state === 'published' ? '<button data-testid="cc-judge">Judge</button>' : ''}</td></tr>`).join('') || '<tr><td class="muted">None yet.</td></tr>'}</table></div>
    <div id="cur-comp-editor"></div><div id="cur-judge"></div>`;
}

async function openCompEditor(id) {
  const t = nowSecs();
  let c = {
    title: '', brief: '', opens_at: t, closes_at: t + 86400, voting_closes_at: t + 2 * 86400, judging_closes_at: t + 3 * 86400,
    weights: { auto: 0.4, votes: 0.3, judges: 0.3 }, requirements: {}, prizes_xp: [300, 200, 100], max_entries_per_user: 1, journey: null, state: 'draft',
  };
  if (id) {
    const r = await api('GET', `/api/curator/competitions/${id}`);
    if (!r.ok) { curatorRefused(r); return; }
    c = r.json;
  }
  const jr = await api('GET', '/api/curator/journeys');
  const journeys = jr.ok ? jr.json.journeys.filter((j) => j.state !== 'archived' || j.id === c.journey) : [];
  const locked = c.state !== 'draft';
  const w = c.weights || {};
  $('cur-judge').innerHTML = '';
  $('cur-comp-editor').innerHTML = `<div class="card" data-testid="comp-editor" data-id="${esc(id || '')}">
    <h3>${id ? 'Edit competition' : 'New competition'} ${id ? `<span class="pill" data-testid="cc-state">${esc(c.state)}</span>` : ''}</h3>
    <label>Title <input id="cc-title" data-testid="cc-title" value="${esc(c.title)}"></label>
    <div>Brief</div><textarea id="cc-brief" data-testid="cc-brief">${esc(c.brief)}</textarea>
    <div id="cc-rules">
    <div class="row"><label>Entries open <input type="datetime-local" id="cc-opens" data-testid="cc-opens" value="${toLocalInput(c.opens_at)}"></label>
      <label>Entries close <input type="datetime-local" id="cc-closes" data-testid="cc-closes" value="${toLocalInput(c.closes_at)}"></label></div>
    <div class="row"><label>Voting closes <input type="datetime-local" id="cc-voting" data-testid="cc-voting" value="${toLocalInput(c.voting_closes_at)}"></label>
      <label>Judging closes (results) <input type="datetime-local" id="cc-judging" data-testid="cc-judging" value="${toLocalInput(c.judging_closes_at)}"></label></div>
    <div class="row">Weights (sum to 1):
      <label>automatic <input type="number" step="any" min="0" id="cc-w-auto" data-testid="cc-w-auto" value="${w.auto ?? 0}"></label>
      <label>votes <input type="number" step="any" min="0" id="cc-w-votes" data-testid="cc-w-votes" value="${w.votes ?? 0}"></label>
      <label>judges <input type="number" step="any" min="0" id="cc-w-judges" data-testid="cc-w-judges" value="${w.judges ?? 0}"></label></div>
    <div class="row"><label>Prize XP, 1st, 2nd, … <input id="cc-prizes" data-testid="cc-prizes" value="${esc((c.prizes_xp || []).join(', '))}"></label>
      <label>Entries per photographer <input type="number" min="1" id="cc-limit" data-testid="cc-limit" value="${c.max_entries_per_user}"></label>
      <label>Prize XP counts toward <select id="cc-journey" data-testid="cc-journey"><option value="">no journey</option>${journeys.map((j) => `<option value="${esc(j.id)}">${esc(j.title)}</option>`).join('')}</select></label></div>
    <fieldset id="cc-req"><legend>Requirements to enter</legend>${reqForm('cc', c.requirements)}</fieldset>
    </div>
    ${locked ? '<p class="muted">Published: only the title and brief can change now.</p>' : ''}
    <div class="row"><button data-testid="cc-save">Save</button>
      ${id ? '<button data-testid="cc-publish">Publish</button><button data-testid="cc-archive">Archive</button>' : ''}</div>
    <div id="cc-msg" data-testid="cc-msg"></div>
  </div>`;
  $('cc-journey').value = c.journey || '';
  initReqForm('cc', c.requirements, false);
  if (locked) for (const el of $('cc-rules').querySelectorAll('input, select')) el.disabled = true;
}

async function saveCompetition() {
  const id = q1('[data-testid=comp-editor]').dataset.id;
  let body = { title: $('cc-title').value, brief: $('cc-brief').value };
  if (!$('cc-opens').disabled) {
    body = {
      ...body,
      opens_at: fromLocalInput($('cc-opens').value), closes_at: fromLocalInput($('cc-closes').value),
      voting_closes_at: fromLocalInput($('cc-voting').value), judging_closes_at: fromLocalInput($('cc-judging').value),
      weights: { auto: Number($('cc-w-auto').value || 0), votes: Number($('cc-w-votes').value || 0), judges: Number($('cc-w-judges').value || 0) },
      prizes_xp: $('cc-prizes').value.split(/[,\s]+/).filter(Boolean).map(Number),
      max_entries_per_user: Number($('cc-limit').value || 1),
      journey: $('cc-journey').value || null,
      requirements: readReqForm('cc'),
    };
  }
  const r = id ? await api('PUT', `/api/curator/competitions/${id}`, body) : await api('POST', '/api/curator/competitions', body);
  if (!r.ok) { setMsg('cc-msg', false, 'Not saved: ' + why(r)); return; }
  await renderCuratorCompetitions();
  await openCompEditor(r.json.id);
  setMsg('cc-msg', true, 'Saved.');
}

async function compState(action) {
  const id = q1('[data-testid=comp-editor]').dataset.id;
  const r = await api('POST', `/api/curator/competitions/${id}/${action}`);
  if (!r.ok) { setMsg('cc-msg', false, `Not ${action}ed: ` + why(r)); return; }
  await renderCuratorCompetitions();
  await openCompEditor(id);
  setMsg('cc-msg', true, action === 'publish' ? 'Published — photographers can enter it.' : 'Archived.');
}

async function openJudge(id, message) {
  const r = await api('GET', `/api/competitions/${id}/leaderboard`);
  $('cur-comp-editor').innerHTML = '';
  if (!r.ok) { $('cur-judge').innerHTML = `<p class="err">${esc(why(r))}</p>`; return; }
  $('cur-judge').innerHTML = `<div class="card" data-testid="judge-panel" data-id="${esc(id)}">
    <h3>Judge entries <span class="pill">${esc(PHASE_WORDS[r.json.phase] || r.json.phase)}</span></h3>
    <div data-testid="judge-msg">${message || ''}</div>
    ${r.json.entries.length ? `<table><tr><th></th><th>photographer</th><th>score now</th><th>judges</th><th>your score (0–10) and note</th></tr>
    ${r.json.entries.map((e) => `<tr data-testid="judge-row" data-entry="${esc(e.entry)}">
      <td>${e.thumb_url ? `<img class="thumb" src="${esc(e.thumb_url)}" alt="">` : '<div class="thumb"></div>'}</td>
      <td>${esc(e.entrant.display_name)}</td><td>${Number(e.score).toFixed(2)}</td><td class="muted">${esc(partText('judges', e.parts.judges))}</td>
      <td><input type="number" min="0" max="10" step="0.5" data-testid="judge-score"> <input data-testid="judge-note" placeholder="note (optional)"> <button data-testid="judge-save">Save score</button></td>
    </tr>`).join('')}</table>` : '<p class="muted">No entries to judge.</p>'}
  </div>`;
}

async function saveJudgement(btn) {
  const panel = btn.closest('[data-testid=judge-panel]');
  const row = btn.closest('[data-testid=judge-row]');
  const score = q1('[data-testid=judge-score]', row).value;
  if (score === '') { q1('[data-testid=judge-msg]', panel).innerHTML = '<span class="err">Give a score from 0 to 10.</span>'; return; }
  const note = q1('[data-testid=judge-note]', row).value.trim();
  const r = await api('PUT', `/api/curator/competitions/${panel.dataset.id}/entries/${row.dataset.entry}/judge`, { score: Number(score), note: note || null });
  await openJudge(panel.dataset.id, r.ok ? `<span class="ok">Score saved: ${esc(score)}.</span>` : `<span class="err">Not saved: ${esc(why(r))}</span>`);
}

// ---- admin -----------------------------------------------------------------------

let users = [];
const emailOf = (subject) => (users.find((u) => u.subject === subject) || {}).email || subject;

async function loadUsers() {
  const r = await api('GET', '/api/admin/users');
  if (!r.ok) { $('admin-msg').textContent = r.status === 403 ? `Refused: ${why(r)}.` : why(r); return false; }
  users = r.json.users;
  return true;
}

async function renderReports() {
  $('admin-msg').textContent = '';
  const r = await api('GET', `/api/admin/reports?state=${encodeURIComponent($('report-state').value)}`);
  if (!r.ok) { $('admin-msg').textContent = r.status === 403 ? `Refused: ${why(r)}.` : why(r); $('reports').innerHTML = ''; return; }
  await loadUsers();
  const rows = await Promise.all(r.json.reports.map(async (rep) => {
    const p = await api('GET', `/api/photos/${rep.photo}`);
    return { rep, photo: p.ok ? p.json : null };
  }));
  $('reports').innerHTML = rows.map(({ rep, photo }) => {
    const hidden = !!(photo && photo.moderation && photo.moderation.hidden);
    const thumb = photo && photo.urls && photo.urls.thumb;
    return `<div class="card" data-testid="report-row" data-id="${esc(rep.id)}" data-photo="${esc(rep.photo)}">
      <div class="row">${thumb ? `<img class="thumb" src="${esc(thumb)}" alt="">` : '<div class="thumb"></div>'}
        <div style="flex: 1; min-width: 14rem"><strong>${esc(rep.reason)}</strong>${rep.note ? ': ' + esc(rep.note) : ''}
          <div class="muted">${esc(photo ? photo.filename : rep.photo)} by ${esc(emailOf(rep.photo_owner))} · reported by ${esc(emailOf(rep.reporter))} · ${esc(fmtTime(rep.created_at))}</div>
          ${hidden ? `<div><span class="pill hidden">hidden</span> ${esc(photo.moderation.reason || '')}</div>` : ''}
          ${rep.state !== 'open' ? `<div class="muted">${esc(rep.state)}${rep.resolution_note ? ': ' + esc(rep.resolution_note) : ''}</div>` : ''}
        </div></div>
      <div class="row">
        ${rep.state === 'open' ? '<input data-testid="dismiss-note" placeholder="note (optional)"><button data-testid="dismiss-btn">Dismiss</button>' : ''}
        ${photo && !hidden ? '<input data-testid="hide-reason" placeholder="reason (required)"><button data-testid="hide-btn">Hide photo</button>' : ''}
        ${hidden ? '<button data-testid="unhide-btn">Unhide photo</button>' : ''}
      </div>
      <div data-testid="report-action-msg"></div>
    </div>`;
  }).join('') || `<p class="muted">No ${esc($('report-state').value)} reports.</p>`;
}

async function reportAction(btn, kind) {
  const row = btn.closest('[data-testid=report-row]');
  const msg = q1('[data-testid=report-action-msg]', row);
  let r;
  if (kind === 'dismiss') {
    const note = q1('[data-testid=dismiss-note]', row).value.trim();
    r = await api('POST', `/api/admin/reports/${row.dataset.id}/dismiss`, { note: note || null });
  } else if (kind === 'hide') {
    const reason = q1('[data-testid=hide-reason]', row).value.trim();
    if (!reason) { msg.innerHTML = '<span class="err">A reason is required — the owner is shown it.</span>'; return; }
    r = await api('POST', `/api/admin/photos/${row.dataset.photo}/hide`, { reason });
  } else {
    r = await api('POST', `/api/admin/photos/${row.dataset.photo}/unhide`, {});
  }
  if (!r.ok) { msg.innerHTML = `<span class="err">Not done: ${esc(why(r))}</span>`; return; }
  await renderReports();
}

async function renderUsers() {
  if (!(await loadUsers())) { $('users').innerHTML = ''; return; }
  const f = ($('user-filter') || {}).value || '';
  const shown = users.filter((u) => !f || u.email.toLowerCase().includes(f.toLowerCase()));
  const keep = f;
  $('users').innerHTML = `<input id="user-filter" data-testid="user-filter" placeholder="filter by email" value="${esc(keep)}">
    <table><tr><th>email</th><th>roles</th><th>status</th><th></th></tr>
    ${shown.map((u) => {
      const cur = u.roles.includes('curator'), adm = u.roles.includes('admin');
      return `<tr data-testid="user-row" data-subject="${esc(u.subject)}" data-email="${esc(u.email)}">
        <td>${esc(u.email)}</td><td data-testid="user-roles">${esc(u.roles.join(', '))}</td>
        <td data-testid="user-status">${u.suspended ? `suspended${u.suspension_reason ? ': ' + esc(u.suspension_reason) : ''}` : 'active'}</td>
        <td><button data-testid="toggle-curator" data-grant="${cur ? '' : '1'}">${cur ? 'Revoke curator' : 'Grant curator'}</button>
          <button data-testid="toggle-admin" data-grant="${adm ? '' : '1'}">${adm ? 'Revoke admin' : 'Grant admin'}</button>
          ${u.suspended ? '<button data-testid="unsuspend-btn">Unsuspend</button>' : '<input data-testid="suspend-reason" placeholder="reason"><button data-testid="suspend-btn">Suspend</button>'}
          <div data-testid="user-msg"></div></td></tr>`;
    }).join('')}</table>`;
}

async function userAction(btn, kind) {
  const row = btn.closest('[data-testid=user-row]');
  const msg = q1('[data-testid=user-msg]', row);
  const s = row.dataset.subject;
  let r;
  if (kind === 'curator' || kind === 'admin') {
    r = await api('POST', `/api/admin/users/${s}/roles`, btn.dataset.grant ? { grant: kind } : { revoke: kind });
  } else if (kind === 'suspend') {
    const reason = q1('[data-testid=suspend-reason]', row).value.trim();
    if (!reason) { msg.innerHTML = '<span class="err">A reason is required.</span>'; return; }
    r = await api('POST', `/api/admin/users/${s}/suspend`, { reason });
  } else {
    r = await api('POST', `/api/admin/users/${s}/unsuspend`, {});
  }
  if (!r.ok) { msg.innerHTML = `<span class="err">Not done: ${esc(why(r))}</span>`; return; }
  await renderUsers();
  if (s === (me || {}).subject) refreshMe();
}

// ---- wiring ------------------------------------------------------------------------

$('login-btn').onclick = login;
$('register-btn').onclick = register;
$('logout-btn').onclick = logout;
$('upload-btn').onclick = upload;
$('gallery').onclick = (e) => {
  const tile = e.target.closest('.tile');
  if (tile) watch(tile.dataset.id);
};
$('nav').onclick = (e) => {
  const b = e.target.closest('button[data-view]');
  if (b) openView(b.dataset.view);
};
$('report-state').onchange = renderReports;
$('reports-refresh').onclick = renderReports;
$('users-refresh').onclick = renderUsers;
$('users').addEventListener('input', (e) => {
  if (e.target.id !== 'user-filter') return;
  const f = e.target.value.toLowerCase();
  for (const row of document.querySelectorAll('[data-testid=user-row]')) row.hidden = !!f && !row.dataset.email.toLowerCase().includes(f);
});
$('cur-tab-journeys').onclick = () => { curatorPane = 'journeys'; renderCurator(); };
$('cur-tab-competitions').onclick = () => { curatorPane = 'competitions'; renderCurator(); };

// Everything the game renders is wired here, by data-testid.
const ACTIONS = {
  'journey-card': (el) => openJourney(el.dataset.id),
  'journeys-back': () => openView('journeys'),
  'quest-row': (el) => { if (el.dataset.state !== 'locked') openQuest(el.dataset.id); },
  'submit-photo-btn': submitToQuest,
  'competition-card': (el) => openCompetition(el.dataset.id),
  'competitions-back': () => openView('competitions'),
  'enter-btn': enterCompetition,
  'star-1': vote, 'star-2': vote, 'star-3': vote, 'star-4': vote, 'star-5': vote,
  'report-btn': showReportForm,
  'report-submit': sendReport,
  'cur-new-journey': () => openJourneyEditor(null),
  'cur-edit-journey': (el) => openJourneyEditor(el.closest('[data-testid=cur-journey-row]').dataset.id),
  'cj-add-level': () => {
    const lv = readLevels();
    lv.push({ level: lv.length + 1, xp: (lv[lv.length - 1] || { xp: 0 }).xp + 100 });
    $('cj-levels').innerHTML = levelsHtml(lv);
  },
  'cj-remove-level': (el) => {
    const i = [...document.querySelectorAll('[data-testid=cj-level]')].indexOf(el.closest('[data-testid=cj-level]'));
    const lv = readLevels();
    lv.splice(i, 1);
    $('cj-levels').innerHTML = levelsHtml(lv);
  },
  'cj-save': saveJourney,
  'cj-publish': () => journeyState('publish'),
  'cj-archive': () => journeyState('archive'),
  'cj-quest-up': (el) => moveQuest(el, -1),
  'cj-quest-down': (el) => moveQuest(el, 1),
  'cj-edit-quest': (el) => openQuestEditor(el.closest('[data-testid=cj-quest]').dataset.id),
  'cj-new-quest': () => openQuestEditor(null),
  'cq-save': saveQuest,
  'cq-publish': () => questState('publish'),
  'cq-archive': () => questState('archive'),
  'cc-new': () => openCompEditor(null),
  'cc-edit': (el) => openCompEditor(el.closest('[data-testid=cur-comp-row]').dataset.id),
  'cc-judge': (el) => openJudge(el.closest('[data-testid=cur-comp-row]').dataset.id),
  'cc-save': saveCompetition,
  'cc-publish': () => compState('publish'),
  'cc-archive': () => compState('archive'),
  'judge-save': saveJudgement,
  'dismiss-btn': (el) => reportAction(el, 'dismiss'),
  'hide-btn': (el) => reportAction(el, 'hide'),
  'unhide-btn': (el) => reportAction(el, 'unhide'),
  'toggle-curator': (el) => userAction(el, 'curator'),
  'toggle-admin': (el) => userAction(el, 'admin'),
  'suspend-btn': (el) => userAction(el, 'suspend'),
  'unsuspend-btn': (el) => userAction(el, 'unsuspend'),
};
document.addEventListener('click', (e) => {
  const el = e.target.closest('button[data-testid], [data-testid=journey-card], [data-testid=quest-row], [data-testid=competition-card]');
  const fn = el && ACTIONS[el.dataset.testid];
  if (fn) fn(el, e);
});

if (token) show();
