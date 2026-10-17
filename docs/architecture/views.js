/* The page's behaviour: the overview, one system at a time, the table of correspondences and the
   boundaries. Inlined into index.html by build.mjs after the map and the engine; it runs in a browser. */
const SYSBY = Object.fromEntries(SYS.map(s => [s.id, s]));
const COR_BY = corBy(COR);
const NAME = id => (SYSBY[id] ? SYSBY[id].name : (SUB.find(s => s.id === id) || ACTORS.find(s => s.id === id) || { t: id }).t);

/* little legend glyphs */
function lineGlyph(p) {
  const dash = PROTO[p].dash;
  const s = S('svg', { class: 'ln', viewBox: '0 0 34 8', 'aria-hidden': 'true' });
  s.append(S('line', { x1: 1, y1: 4, x2: 33, y2: 4, 'stroke-dasharray': dash || null }));
  return s;
}
function legendItems(withLines) {
  const out = KINDS.map(([k, t]) => H('li', null, [H('span', { class: 'sw ' + k }), t]));
  if (withLines) for (const p of Object.keys(PROTO)) out.push(H('li', null, [lineGlyph(p), PROTO[p].name]));
  return out;
}

/* ---------- overview ---------- */
const OV = { svg: null, rects: {}, edges: [] };

function flowChips(items, maxW) {
  const out = []; let x = 0, y = 0;
  for (const [k, label] of items) {
    const w = Math.ceil(tw(label, FONT.chip)) + 14;
    if (x && x + w > maxW) { x = 0; y += 24; }
    out.push({ k, label, x, y, w }); x += w + 6;
  }
  return { out, rows: out.length ? out[out.length - 1].y / 24 + 1 : 0 };
}

function buildOverview(go) {
  const W = CANVAS_W, X = [30, 490, 950], CWD = 300, GAPY = 104;
  // actors strip
  const aw = 290, ay = 20, ah = 56;
  const actors = ACTORS.map((a, i) => ({ id: 'act-' + a.id, a, x: 30 + i * (aw + 20), y: ay, w: aw, h: ah }));
  // cards: measure
  const cards = SYS.map(s => {
    const [c, r] = OV_POS[s.id];
    const pl = wrap(s.purpose, CWD - 32, FONT.cp);
    const fl = flowChips(s.parts, CWD - 32);
    const h = 18 + 22 + 8 + pl.length * 16 + 10 + fl.rows * 24 + 6 + 24 + 14;
    return { id: s.id, s, c, r, pl, fl, h, w: CWD };
  });
  const rowH = [0, 1, 2].map(r => Math.max(...cards.filter(c => c.r === r).map(c => c.h)));
  const rowY = []; let y = ay + ah + 74;
  for (let r = 0; r < 3; r++) { rowY[r] = y; y += rowH[r] + GAPY; }
  for (const c of cards) { c.x = X[c.c]; c.y = rowY[c.r]; c.h = rowH[c.r]; }
  // substrate band
  const bandY = y - GAPY + 46, sw = 162, sg = 14;
  const subs = SUB.map((s, i) => {
    const lines = s.s.flatMap(l => wrap(l, sw - 24, FONT.s));
    return { id: 'sub-' + s.id, s, lines, x: 30 + i * (sw + sg), y: bandY + 30, w: sw, h: 14 + 22 + lines.length * 15 + 10 };
  });
  const subH = Math.max(...subs.map(s => s.h)); subs.forEach(s => s.h = subH);
  const H_ = bandY + 30 + subH + 26;

  const rects = {};
  cards.forEach(c => rects[c.id] = c);
  actors.forEach(a => rects[a.id] = a);
  subs.forEach(s => rects[s.id] = s);
  const edges = OV_EDGES.map(e => ({ a: e.a, b: e.b, as: e.as, bs: e.bs, lab: e.lab, p: e.p, d: e.d, sys: [e.a, e.b] }));
  routeEdges(edges, rects, W, H_);

  const svg = S('svg', { viewBox: `0 0 ${W} ${H_}`, role: 'img', 'aria-label': 'The systems and their primary correspondences' });
  // bands
  svg.append(S('rect', { class: 'band', x: 14, y: bandY - 6, width: W - 28, height: subH + 66, rx: 12 }));
  svg.append(S('text', { class: 'band-t', x: 30, y: bandY + 14 }, 'Shared state the systems sit on'));
  svg.append(S('text', { class: 'band-t', x: 30, y: ay - 6 }, 'Who uses it'));
  // edges under cards
  const gE = S('g'); const placed = [];
  Object.values(rects).forEach(r => placed.push({ x: r.x - 2, y: r.y - 2, w: r.w + 4, h: r.h + 4 }));
  drawEdges(edges, gE, placed); svg.append(gE);

  // actors
  const touchEls = {};
  for (const a of actors) {
    const n = { id: a.id, x: a.x, y: a.y, w: a.w, h: a.h, kind: 'actor', t: a.a.t, lines: [a.a.s], ghost: false };
    const g = drawNode(n, svg); g.classList.add('sub-node'); touchEls[a.a.id] = g;
    g.addEventListener('pointerenter', () => focusSys(svg, a.a.touch, g, true));
    g.addEventListener('pointerleave', () => focusSys(svg, null));
  }
  // substrates
  const subEls = {};
  for (const s of subs) {
    const n = { id: s.id, x: s.x, y: s.y, w: s.w, h: s.h, kind: 'store', t: s.s.t, lines: s.lines };
    const g = drawNode(n, svg); g.classList.add('sub-node'); subEls[s.s.id] = g;
    const users = SYS.filter(x => x.uses.includes(s.s.id)).map(x => x.id);
    g.addEventListener('pointerenter', () => focusSys(svg, users, g, true));
    g.addEventListener('pointerleave', () => focusSys(svg, null));
  }
  // cards
  for (const c of cards) {
    const g = S('g', { class: 'card', 'data-id': c.id, tabindex: 0, role: 'link', 'aria-label': 'Open ' + c.s.name + ' in isolation' });
    g.append(S('rect', { class: 'frame', x: c.x, y: c.y, width: c.w, height: c.h, rx: 12 }));
    g.append(S('text', { class: 'ct', x: c.x + 16, y: c.y + 30 }, c.s.name));
    g.append(S('text', { class: 'go', x: c.x + c.w - 14, y: c.y + 29, 'text-anchor': 'end' }, 'isolate ›'));
    c.pl.forEach((l, i) => g.append(S('text', { class: 'cp', x: c.x + 16, y: c.y + 52 + i * 16 }, l)));
    const cy = c.y + 18 + 22 + 8 + c.pl.length * 16 + 10;
    for (const ch of c.fl.out) {
      const cg = S('g', { class: 'chip k-' + ch.k });
      cg.append(S('rect', { x: c.x + 16 + ch.x, y: cy + ch.y, width: ch.w, height: 19, rx: 5 }));
      cg.append(S('text', { x: c.x + 16 + ch.x + 7, y: cy + ch.y + 13 }, ch.label));
      g.append(cg);
    }
    // uses
    const uy = c.y + c.h - 30;
    g.append(S('text', { class: 'lbl-uses', x: c.x + 16, y: uy + 13 }, 'on'));
    let ux = c.x + 38;
    for (const u of c.s.uses) {
      const chip = SUB.find(z => z.id === u).chip, w = Math.ceil(tw(chip, '500 9.5px "IBM Plex Mono", monospace')) + 12;
      const ug = S('g', { class: 'use' });
      ug.append(S('rect', { x: ux, y: uy, width: w, height: 18, rx: 5 }), S('text', { x: ux + 6, y: uy + 12.5 }, chip));
      g.append(ug); ux += w + 5;
    }
    const open = () => go(c.id);
    g.addEventListener('click', open);
    g.addEventListener('keydown', ev => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); open(); } });
    g.addEventListener('pointerenter', () => focusSys(svg, [c.id], g));
    g.addEventListener('pointerleave', () => focusSys(svg, null));
    g.addEventListener('focus', () => focusSys(svg, [c.id], g));
    g.addEventListener('blur', () => focusSys(svg, null));
    svg.append(g);
  }
  OV.svg = svg; OV.edges = edges;
  return svg;
}

/* Light up a set of systems and every correspondence that touches one of them. */
function focusSys(svg, ids, self, only) {
  if (!ids) { svg.removeAttribute('data-focus'); $$('.rel,.sub-on', svg).forEach(e => e.classList.remove('rel', 'sub-on')); $$('.edge.on', svg).forEach(e => e.classList.remove('on')); return; }
  svg.setAttribute('data-focus', ids.join(','));
  const set = new Set(ids);
  const related = new Set(ids);
  $$('.edge', svg).forEach(e => {
    const sys = (e.getAttribute('data-sys') || '').split(' ').filter(Boolean);
    const hit = only ? sys.length > 0 && sys.every(s => set.has(s)) : sys.some(s => set.has(s));
    e.classList.toggle('on', hit);
    if (hit && !only) sys.forEach(s => related.add(s));
  });
  $$('.card', svg).forEach(c => c.classList.toggle('rel', related.has(c.getAttribute('data-id'))));
  if (self && !self.classList.contains('card')) self.classList.add('sub-on');
  $$('.sub-node', svg).forEach(n => n.classList.toggle('rel', n === self));
}

/* ---------- isolation ---------- */
const ISO = { current: null };
function protoBadge(p, text) {
  return H('span', { class: 'pb' }, [lineGlyph(p), text]);
}
function whoCell(side) {
  const [id, part] = side;
  const isSys = !!SYSBY[id];
  const name = NAME(id);
  const inner = [H('b', null, name), H('span', null, part)];
  if (isSys) return H('button', { class: 'row-link', type: 'button', onclick: () => openSystem(id), 'aria-label': 'Open ' + name }, H('div', { class: 'who' }, inner));
  return H('div', { class: 'who' }, inner);
}

function buildIsoTable(sysId) {
  const tb = H('tbody');
  for (const r of isoRows(sysId, DIA[sysId], COR)) {
    const c = r.c;
    const tr = H('tr', { 'data-n': r.ns.join(' ') || null });
    tr.append(
      H('td', { class: 'n' }, r.ns.map(n => H('span', { class: 'bdg' }, String(n)))),
      H('td', null, H('div', { class: 'who' }, [H('b', null, r.here[1])])),
      H('td', { class: 'arrow' }, r.arrow),
      H('td', null, whoCell(r.other)),
      H('td', null, protoBadge(c.p, c.over + (c.internal ? ' \u00b7 inside' : ''))),
      H('td', null, c.contract),
      H('td', null, H('span', { class: 'adr' }, c.adr ? 'ADR ' + c.adr : ''))
    );
    tb.append(tr);
  }
  const t = H('table', null, [
    H('thead', null, H('tr', null, ['', 'Here', '', 'Peer', 'Over', 'What crosses', 'Decided in'].map(h => H('th', null, h)))),
    tb
  ]);
  return { wrap: H('div', { class: 'tbl-wrap' }, t), tb };
}

function renderIso(sysId, append) {
  const s = SYSBY[sysId]; ISO.current = sysId;
  $$('.sys-btn', $('#rail')).forEach(b => b.setAttribute('aria-current', b.dataset.id === sysId ? 'true' : 'false'));
  const main = $('#iso-main'); if (!append) main.replaceChildren();
  main.append(H('div', { class: 'iso-head' }, [
    H('h2', null, s.name), H('p', null, s.blurb),
    H('p', { class: 'entry' }, [H('b', null, 'Reached through '), s.entry])
  ]));
  const { svg, edges } = buildIsolation(sysId, openSystem);
  main.append(H('ul', { class: 'legend' }, legendItems(true)));
  main.append(H('div', { class: 'fig' }, svg));
  main.append(H('p', { class: 'hint' }, 'Solid boxes are this system. Dashed boxes are its peers: open one to see it alone. Numbers on the lines match the table; hover a row to find its line.'));
  const tbl = buildIsoTable(sysId);
  main.append(H('div', null, [H('h3', { class: 'sec' }, 'Correspondences'), tbl.wrap]));
  // linking rows <-> lines
  const on = (ns, v) => {
    ns.split(' ').forEach(n => { $$('.edge[data-n="' + n + '"]', svg).forEach(e => e.classList.toggle('on', v)); });
    $$('tr[data-n]', tbl.tb).forEach(r => { if (r.dataset.n.split(' ').some(n => ns.split(' ').includes(n))) r.classList.toggle('on', v); });
    if (v) svg.setAttribute('data-focus', ns); else svg.removeAttribute('data-focus');
  };
  $$('tr[data-n]', tbl.tb).forEach(r => { const n = r.dataset.n; r.addEventListener('pointerenter', () => on(n, true)); r.addEventListener('pointerleave', () => on(n, false)); });
  $$('.edge[data-n]', svg).forEach(e => { const n = e.dataset.n; e.addEventListener('pointerenter', () => on(n, true)); e.addEventListener('pointerleave', () => on(n, false)); });
  main.append(H('div', { class: 'cols' }, [
    H('section', { class: 'box' }, [H('h3', null, 'What keeps it apart'), H('ul', null, s.boundary.map(b => H('li', null, b)))]),
    H('section', { class: 'box' }, [H('h3', null, 'Where it lives'), H('div', { class: 'codes' }, s.code.map(c => H('code', null, c)))])
  ]));
}

function buildRail() {
  const rail = $('#rail');
  for (const s of SYS) rail.append(H('button', { class: 'sys-btn', type: 'button', 'data-id': s.id, title: s.short, onclick: () => openSystem(s.id) }, s.name));
}

/* ---------- correspondences ---------- */
const CF = { sys: 'all', protos: new Set(), q: '', inside: false };
function buildCor() {
  const f = $('#cor-filters'); f.replaceChildren();
  const sysRow = H('div', { class: 'filters' }, [H('label', null, 'System')]);
  const mk = (id, label) => H('button', { class: 'chip-btn', type: 'button', 'aria-pressed': String(CF.sys === id), onclick: () => { CF.sys = id; buildCor(); } }, label);
  sysRow.append(mk('all', 'All'));
  for (const s of SYS) sysRow.append(mk(s.id, s.name));
  const protoRow = H('div', { class: 'filters' }, [H('label', null, 'Over')]);
  for (const p of Object.keys(PROTO)) protoRow.append(H('button', { class: 'chip-btn', type: 'button', 'aria-pressed': String(CF.protos.has(p)), onclick: () => { CF.protos.has(p) ? CF.protos.delete(p) : CF.protos.add(p); buildCor(); } }, PROTO[p].name));
  protoRow.append(H('button', { class: 'chip-btn', type: 'button', 'aria-pressed': String(CF.inside), onclick: () => { CF.inside = !CF.inside; buildCor(); } }, 'Include rows inside one system'));
  const q = H('input', { class: 'search', type: 'search', id: 'cor-q', placeholder: 'Search a part or subject', 'aria-label': 'Search correspondences', value: CF.q });
  const rows = COR.filter(c => (CF.inside || !c.internal)
    && (CF.sys === 'all' || c.a[0] === CF.sys || c.b[0] === CF.sys)
    && (!CF.protos.size || CF.protos.has(c.p))
    && (!CF.q || (c.a.join(' ') + ' ' + c.b.join(' ') + ' ' + c.contract + ' ' + c.over + ' ' + c.adr + ' ' + NAME(c.a[0]) + ' ' + NAME(c.b[0])).toLowerCase().includes(CF.q.toLowerCase())));
  const count = H('span', { class: 'count' }, rows.length + ' of ' + COR.filter(c => CF.inside || !c.internal).length);
  const searchRow = H('div', { class: 'filters' }, [q, count]);
  f.append(sysRow, protoRow, searchRow);
  q.addEventListener('input', () => { CF.q = q.value; renderCorRows(); const el = $('#cor-q'); if (el) { el.focus(); } });
  renderCorRows();
}
function renderCorRows() {
  const rows = COR.filter(c => (CF.inside || !c.internal)
    && (CF.sys === 'all' || c.a[0] === CF.sys || c.b[0] === CF.sys)
    && (!CF.protos.size || CF.protos.has(c.p))
    && (!CF.q || (c.a.join(' ') + ' ' + c.b.join(' ') + ' ' + c.contract + ' ' + c.over + ' ' + c.adr + ' ' + NAME(c.a[0]) + ' ' + NAME(c.b[0])).toLowerCase().includes(CF.q.toLowerCase())));
  const cnt = $('#cor-filters .count'); if (cnt) cnt.textContent = rows.length + ' of ' + COR.filter(c => CF.inside || !c.internal).length;
  const t = $('#cor-table'); t.replaceChildren();
  t.append(H('thead', null, H('tr', null, ['From', '', 'To', 'Over', 'What crosses', 'Decided in'].map(h => H('th', null, h)))));
  const tb = H('tbody');
  if (!rows.length) tb.append(H('tr', null, H('td', { colspan: 6 }, 'Nothing matches. Clear a filter.')));
  for (const c of rows) {
    tb.append(H('tr', null, [
      H('td', null, whoCell(c.a)), H('td', { class: 'arrow' }, '→'), H('td', null, whoCell(c.b)),
      H('td', null, protoBadge(c.p, c.over)), H('td', null, c.contract),
      H('td', null, H('span', { class: 'adr' }, c.adr ? 'ADR ' + c.adr : ''))
    ]));
  }
  t.append(tb);
}

/* ---------- boundaries ---------- */
function buildBoundaries() {
  const v = $('#view-bnd'); v.replaceChildren();
  v.append(H('div', { class: 'rule-box' }, [H('b', null, RULE.t), H('small', null, RULE.note)]));
  const items = list => H('div', { class: 'items' }, list.map(t => H('span', null, t)));
  const tenants = H('div', { class: 'tenants' }, TENANTS.names.map(o => H('div', { class: 'tenant' }, [
    H('h4', null, o + ' \u00b7 component instances'),
    H('p', null, TENANTS.text)
  ])));
  // Each zone sits inside the one before it, so build from the inside out; the innermost holds the tenants.
  let inner = null;
  for (let i = ZONES.length - 1; i >= 0; i--) {
    const z = ZONES[i];
    const body = [];
    if (z.items.length) body.push(items(z.items));
    body.push(inner || tenants);
    inner = H('section', { class: 'zone z' + (i + 1) }, [H('div', { class: 'zh' }, [H('h3', null, z.t), H('p', null, z.d)]), ...body]);
  }
  v.append(H('div', { class: 'zones' }, inner));
  const gn = H('table', null, [H('thead', null, H('tr', null, ['The guest names', 'The host resolves it to', 'ADR'].map(h => H('th', null, h)))),
    H('tbody', null, GUEST_NAMES.map(r => H('tr', null, [H('td', null, r[0]), H('td', null, r[1]), H('td', null, H('span', { class: 'adr' }, r[2]))])))]);
  const cr = H('table', null, [H('thead', null, H('tr', null, ['Credential', 'Held by', 'Guards', 'ADR'].map(h => H('th', null, h)))),
    H('tbody', null, CREDS.map(r => H('tr', null, [H('td', null, H('b', null, r[0])), H('td', null, r[1]), H('td', null, r[2]), H('td', null, H('span', { class: 'adr' }, r[3]))])))]);
  v.append(H('div', { class: 'split' }, [
    H('div', null, [H('h3', { class: 'sec' }, 'What the guest names, and what the host makes of it'), H('div', { class: 'tbl-wrap' }, gn)]),
    H('div', null, [H('h3', { class: 'sec' }, 'Who holds what'), H('div', { class: 'tbl-wrap' }, cr)])
  ]));
}

/* ---------- routing between views ---------- */
const VIEWS = ['map', 'iso', 'cor', 'bnd'];
let built = false;
function show(view) {
  VIEWS.forEach(v => { $('#view-' + v).hidden = v !== view; $('#tab-' + v).setAttribute('aria-selected', String(v === view)); });
}
function openSystem(id) {
  if (!SYSBY[id]) return;
  renderIso(id); show('iso');
  try { history.replaceState(null, '', '#' + id); } catch (e) {}
  window.scrollTo({ top: 0 });
}
function openView(v) {
  if (v === 'iso') { renderIso(ISO.current || SYS[0].id); }
  show(v);
  try { history.replaceState(null, '', '#' + ({ map: 'map', iso: ISO.current || 'library', cor: 'correspondences', bnd: 'boundaries' })[v]); } catch (e) {}
}

function buildAll() {
  $('#legend-map').replaceChildren(...legendItems(true));
  const fig = $('#fig-map'); fig.replaceChildren(buildOverview(openSystem));
  if (!built) { buildRail(); }
  built = true;
  buildCor(); buildBoundaries();
  if (ISO.current) renderIso(ISO.current);
}

function route() {
  const h = (location.hash || '').slice(1);
  if (SYSBY[h]) { renderIso(h); show('iso'); }
  else if (h === 'correspondences') show('cor');
  else if (h === 'boundaries') show('bnd');
  else if (h === 'all') { /* review mode: every view stacked, every system in turn */ VIEWS.forEach(v => { $('#view-' + v).hidden = false; }); $('#iso-main').replaceChildren(); SYS.forEach(x => renderIso(x.id, true)); }
  else show('map');
}

function init() {
  $('#prov').textContent = 'Last revised after ADR-' + META.revisedAfter + '. Read from the README, docs/CURRENT.md, the ADRs and the source header of each binary.';
  $$('.tab').forEach(t => t.addEventListener('click', () => openView(t.dataset.view)));
  buildAll(); route();
  window.addEventListener('hashchange', route);
  if (document.fonts && document.fonts.ready) document.fonts.ready.then(() => { buildAll(); route(); });
}
init();
