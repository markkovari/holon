/* The page's drawing code: text is measured with the real fonts, boxes are laid out on a grid,
   and every line is routed around the boxes on a 10px lattice, so a diagram is data and not
   hand-placed coordinates. Inlined into index.html by build.mjs; it runs in a browser, not in Node. */
const NS = 'http://www.w3.org/2000/svg';
const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
function S(tag, attrs, kids) {
  const e = document.createElementNS(NS, tag);
  if (attrs) for (const k in attrs) { const v = attrs[k]; if (v !== null && v !== undefined && v !== false) e.setAttribute(k, v === true ? '' : v); }
  if (kids != null) for (const c of [].concat(kids)) { if (c == null) continue; e.append(c.nodeType ? c : document.createTextNode(String(c))); }
  return e;
}
function H(tag, attrs, kids) {
  const e = document.createElement(tag);
  if (attrs) for (const k in attrs) {
    const v = attrs[k]; if (v === null || v === undefined || v === false) continue;
    if (k === 'class') e.className = v; else if (k.startsWith('on')) e.addEventListener(k.slice(2), v); else e.setAttribute(k, v === true ? '' : v);
  }
  if (kids != null) for (const c of [].concat(kids)) { if (c == null) continue; e.append(c.nodeType ? c : document.createTextNode(String(c))); }
  return e;
}

/* ---- measuring ---- */
const FONT = {
  t:  '600 13px "IBM Plex Sans", ui-sans-serif, system-ui, sans-serif',
  tm: '500 12.5px "IBM Plex Mono", ui-monospace, Menlo, monospace',
  s:  '400 11.5px "IBM Plex Sans", ui-sans-serif, system-ui, sans-serif',
  cp: '400 12px "IBM Plex Sans", ui-sans-serif, system-ui, sans-serif',
  ct: '600 15px "IBM Plex Sans", ui-sans-serif, system-ui, sans-serif',
  chip: '500 10.5px "IBM Plex Mono", ui-monospace, Menlo, monospace',
  lab: '500 10.5px "IBM Plex Mono", ui-monospace, Menlo, monospace'
};
const _cv = document.createElement('canvas').getContext('2d');
function tw(t, f) { _cv.font = f; return _cv.measureText(t).width; }
function wrap(text, maxW, f) {
  const words = String(text).split(' '), out = []; let cur = '';
  for (const w of words) {
    const test = cur ? cur + ' ' + w : w;
    if (!cur || tw(test, f) <= maxW) cur = test; else { out.push(cur); cur = w; }
  }
  if (cur) out.push(cur);
  return out;
}

/* ---- grid layout for an isolation diagram ---- */
const COLS = [276, 545, 814], CW = 190, GX_L = 16, GX_R = 1104, GW = 160, CANVAS_W = 1280;

function layoutDiagram(d) {
  const rects = {};
  const rows = {};
  const cols = d.cols || COLS, cw = d.cw || CW, CWID = d.W || CANVAS_W;
  // size nodes
  for (const n of d.nodes) {
    if (n.ghost) n.kind = 'ext';
    const cs = n.cs || 1;
    n.w = cols[n.c + cs - 1] + cw - cols[n.c];
    const inner = n.w - 30;
    n.lines = n.s.flatMap(l => wrap(l, inner, FONT.s));
    n.h = Math.max(62, 14 + 22 + n.lines.length * 15 + 11);
    if (!n.ghost) n.kind = n.k;
    rows[n.r] = Math.max(rows[n.r] || 0, n.h);
  }
  const nrows = Math.max(...d.nodes.map(n => n.r)) + 1;
  const gaps = Array.isArray(d.rowGap) ? d.rowGap : null;
  const hasTop = (d.ghosts || []).some(g => g.side === 't');
  const top = hasTop ? 36 + 84 : 36;
  const rowY = [];
  let y = top;
  for (let r = 0; r < nrows; r++) {
    if (r > 0) y += (gaps ? gaps[r] : 74);
    rowY[r] = y; y += rows[r] || 62;
  }
  for (const n of d.nodes) {
    n.x = cols[n.c]; n.y = rowY[n.r] + ((rows[n.r] - n.h) / 2); // centre in the row; equal rows look aligned
    n.h = n.h; rects[n.id] = n;
  }
  // ghosts
  for (const g of d.ghosts || []) {
    g.lines = g.s.flatMap(l => wrap(l, GW - 28, FONT.s));
    g.w = GW;
    g.h = g.h || Math.max(48, 12 + 20 + g.lines.length * 14 + 8);
    if (g.side === 't') { g.y = 14; }
    else {
      g.x = g.side === 'l' ? GX_L : CWID - 16 - GW;
      if (g.y == null) { const r = g.r || 0; g.y = rowY[r] + ((rows[r] || 62) - g.h) / 2 + (g.dy || 0); }
    }
    g.ghost = true; g.kind = g.actor ? 'actor' : g.store ? 'store' : 'ext';
    rects[g.id] = g;
  }
  // groups
  const groups = [];
  for (const gr of d.groups || []) {
    const ns = gr.ids.map(id => rects[id]);
    const x0 = Math.min(...ns.map(n => n.x)) - 14, y0 = Math.min(...ns.map(n => n.y)) - 32;
    const x1 = Math.max(...ns.map(n => n.x + n.w)) + 14, y1 = Math.max(...ns.map(n => n.y + n.h)) + 14;
    groups.push({ id: gr.id, t: gr.t, x: x0, y: y0, w: x1 - x0, h: y1 - y0 });
    rects['grp:' + gr.id] = { id: 'grp:' + gr.id, x: x0, y: y0, w: x1 - x0, h: y1 - y0, noBlock: true, hidden: true };
  }
  const all = Object.values(rects).filter(r => !r.hidden);
  const H = Math.max(...all.map(r => r.y + r.h)) + 34;
  return { rects, groups, W: CWID, H };
}

/* ---- routing ---- */
const CELL = 10, PAD = 9;
function newRouter(W, H, rects) {
  const gw = Math.ceil(W / CELL), gh = Math.ceil(H / CELL);
  const blocked = new Uint8Array(gw * gh);
  for (const r of rects) {
    if (r.noBlock) continue;
    const x0 = Math.max(0, Math.floor((r.x - PAD) / CELL)), x1 = Math.min(gw - 1, Math.floor((r.x + r.w + PAD - 1) / CELL));
    const y0 = Math.max(0, Math.floor((r.y - PAD) / CELL)), y1 = Math.min(gh - 1, Math.floor((r.y + r.h + PAD - 1) / CELL));
    for (let y = y0; y <= y1; y++) for (let x = x0; x <= x1; x++) blocked[y * gw + x] = 1;
  }
  return { gw, gh, blocked, usedH: new Uint8Array(gw * gh), usedV: new Uint8Array(gw * gh), W, H };
}
const DX = [1, 0, -1, 0], DY = [0, 1, 0, -1];   // 0 right, 1 down, 2 left, 3 up
const snap = v => Math.floor(v / CELL) * CELL + CELL / 2;

function sideAnchor(r, side, t) {
  const clampv = (v, a, b) => Math.max(a, Math.min(b, v));
  if (side === 'l' || side === 'r') {
    const y = snap(clampv(r.y + r.h * t, r.y + 9, r.y + r.h - 9));
    return { x: side === 'l' ? r.x : r.x + r.w, y, dir: side === 'l' ? 2 : 0 };
  }
  const x = snap(clampv(r.x + r.w * t, r.x + 9, r.x + r.w - 9));
  return { x, y: side === 't' ? r.y : r.y + r.h, dir: side === 't' ? 3 : 1 };
}

function portCell(R, a) {
  // first free cell outside the (inflated) box, in the anchor's outward direction
  let cx = Math.floor(a.x / CELL), cy = Math.floor(a.y / CELL);
  for (let i = 0; i < 6; i++) {
    cx += DX[a.dir]; cy += DY[a.dir];
    if (cx < 0 || cy < 0 || cx >= R.gw || cy >= R.gh) break;
    if (!R.blocked[cy * R.gw + cx]) break;
  }
  return { x: cx, y: cy };
}

class Heap {
  constructor() { this.a = []; }
  push(k, v) { const a = this.a; a.push([k, v]); let i = a.length - 1; while (i > 0) { const p = (i - 1) >> 1; if (a[p][0] <= a[i][0]) break; [a[p], a[i]] = [a[i], a[p]]; i = p; } }
  pop() { const a = this.a, top = a[0], last = a.pop(); if (a.length) { a[0] = last; let i = 0; for (;;) { let l = 2 * i + 1, r = l + 1, m = i; if (l < a.length && a[l][0] < a[m][0]) m = l; if (r < a.length && a[r][0] < a[m][0]) m = r; if (m === i) break; [a[m], a[i]] = [a[i], a[m]]; i = m; } } return top; }
  get size() { return this.a.length; }
}

function findPath(R, s, t, sdir) {
  const { gw, gh, blocked, usedH, usedV } = R;
  const TURN = 6, OVER = 9, CROSS = 3;
  const key = (x, y, d) => ((y * gw + x) << 2) | d;
  const best = new Map(), prev = new Map();
  const h = new Heap();
  const k0 = key(s.x, s.y, sdir);
  best.set(k0, 0); h.push(Math.abs(s.x - t.x) + Math.abs(s.y - t.y), [s.x, s.y, sdir, 0]);
  let guard = 0;
  while (h.size && guard++ < 400000) {
    const [, [x, y, d, g]] = h.pop();
    if (g > (best.get(key(x, y, d)) ?? Infinity)) continue;
    if (x === t.x && y === t.y) {
      const path = [[x, y]]; let k = key(x, y, d);
      while (prev.has(k)) { const pk = prev.get(k); const cell = pk >> 2; path.push([cell % gw, (cell / gw) | 0]); k = pk; }
      return path.reverse();
    }
    for (let nd = 0; nd < 4; nd++) {
      if (nd === ((d + 2) & 3)) continue;
      const nx = x + DX[nd], ny = y + DY[nd];
      if (nx < 0 || ny < 0 || nx >= gw || ny >= gh) continue;
      const ci = ny * gw + nx;
      if (blocked[ci] && !(nx === t.x && ny === t.y) && !(nx === s.x && ny === s.y)) continue;
      let step = 1 + (nd !== d ? TURN : 0);
      const horiz = (nd & 1) === 0;
      if (horiz ? usedH[ci] : usedV[ci]) step += OVER; else if (horiz ? usedV[ci] : usedH[ci]) step += CROSS;
      const ng = g + step, nk = key(nx, ny, nd);
      if (ng < (best.get(nk) ?? Infinity)) { best.set(nk, ng); prev.set(nk, key(x, y, d)); h.push(ng + Math.abs(nx - t.x) + Math.abs(ny - t.y), [nx, ny, nd, ng]); }
    }
  }
  return null;
}

function simplify(pts) {
  const out = [];
  for (const p of pts) {
    if (out.length && out[out.length - 1][0] === p[0] && out[out.length - 1][1] === p[1]) continue;
    out.push(p);
    while (out.length >= 3) {
      const [a, b, c] = out.slice(-3);
      if ((a[0] === b[0] && b[0] === c[0]) || (a[1] === b[1] && b[1] === c[1])) out.splice(out.length - 2, 1); else break;
    }
  }
  return out;
}

function chooseSides(a, b) {
  if (a.side === 't') return ['b', 't'];
  if (b.side === 't') return ['t', 'b'];
  if (b.x >= a.x + a.w + 8) return ['r', 'l'];
  if (b.x + b.w <= a.x - 8) return ['l', 'r'];
  if (b.y >= a.y + a.h) return ['b', 't'];
  return ['t', 'b'];
}

/* Route a list of edge specs ({a,b,as?,bs?}) over rects. Returns edges with .pts. */
function routeEdges(edges, rects, W, H) {
  const R = newRouter(W, H, Object.values(rects));
  // sides + port fractions
  for (const e of edges) {
    const A = rects[e.a], B = rects[e.b];
    const [as, bs] = chooseSides(A, B);
    e.as = e.as || as; e.bs = e.bs || bs;
  }
  const bySide = {};
  edges.forEach((e, i) => {
    for (const end of ['a', 'b']) {
      const id = e[end], side = e[end + 's'], other = rects[e[end === 'a' ? 'b' : 'a']];
      (bySide[id + '|' + side] = bySide[id + '|' + side] || []).push({ e, end, ord: (side === 'l' || side === 'r') ? other.y + other.h / 2 : other.x + other.w / 2 });
    }
  });
  for (const k in bySide) {
    const list = bySide[k].sort((p, q) => p.ord - q.ord);
    list.forEach((it, i) => { it.e[it.end + 't'] = (i + 1) / (list.length + 1); });
  }
  // short edges first so long ones bend around them
  const order = edges.map((e, i) => i).sort((i, j) => {
    const L = e => { const A = rects[e.a], B = rects[e.b]; return Math.abs(A.x - B.x) + Math.abs(A.y - B.y); };
    return L(edges[i]) - L(edges[j]);
  });
  for (const i of order) {
    const e = edges[i], A = rects[e.a], B = rects[e.b];
    const pa = sideAnchor(A, e.as, e.at), pb = sideAnchor(B, e.bs, e.bt);
    const sa = portCell(R, pa), sb = portCell(R, pb);
    let cells = findPath(R, sa, sb, pa.dir);
    let pts;
    if (cells) {
      const mid = cells.map(([x, y]) => [x * CELL + CELL / 2, y * CELL + CELL / 2]);
      pts = simplify([[pa.x, pa.y], ...mid, [pb.x, pb.y]]);
      // mark use
      for (let k = 1; k < cells.length; k++) {
        const [x0, y0] = cells[k - 1], [x1, y1] = cells[k];
        const ci = y1 * R.gw + x1, cj = y0 * R.gw + x0;
        if (y0 === y1) { R.usedH[ci] = 1; R.usedH[cj] = 1; } else { R.usedV[ci] = 1; R.usedV[cj] = 1; }
      }
    } else {
      pts = simplify([[pa.x, pa.y], [(pa.x + pb.x) / 2, pa.y], [(pa.x + pb.x) / 2, pb.y], [pb.x, pb.y]]);
    }
    e.pts = pts;
  }
  return edges;
}

/* ---- drawing ---- */
function headPath(p, q, size = 8) {
  // arrowhead at q, coming from p
  const dx = Math.sign(q[0] - p[0]), dy = Math.sign(q[1] - p[1]);
  const w = size * .55;
  if (dx) return `M${q[0]},${q[1]} L${q[0] - dx * size},${q[1] - w} L${q[0] - dx * size},${q[1] + w} Z`;
  return `M${q[0]},${q[1]} L${q[0] - w},${q[1] - dy * size} L${q[0] + w},${q[1] - dy * size} Z`;
}

const _labels = [];
function placeLabel(e, placed) {
  if (!e.lab && !e.n) return null;
  const pts = e.pts;
  const bw = e.n ? 22 : 0;
  const text = e.lab || '';
  const w = (text ? tw(text, FONT.lab) : 0) + bw + (e.n && text ? 4 : 0);
  // candidate segments, longest first
  const segs = [];
  for (let i = 1; i < pts.length; i++) {
    const [x0, y0] = pts[i - 1], [x1, y1] = pts[i];
    segs.push({ i, len: Math.abs(x1 - x0) + Math.abs(y1 - y0), horiz: y0 === y1, x0, y0, x1, y1 });
  }
  segs.sort((a, b) => b.len - a.len);
  const overlap = (r, list) => list.some(o => !(r.x + r.w < o.x || o.x + o.w < r.x || r.y + r.h < o.y || o.y + o.h < r.y));
  for (const sg of segs) {
    const need = sg.horiz ? w + 14 : 20;
    if (sg.len < Math.min(need, 40)) continue;
    const mx = (sg.x0 + sg.x1) / 2, my = (sg.y0 + sg.y1) / 2;
    for (const off of [0, 1, -1, 2, -2, 3, -3]) {
      const dx = sg.horiz ? off * (w * .55 + 6) : 0, dy = sg.horiz ? 0 : off * 22;
      const cx = mx + dx, cy = my + dy;
      // keep inside the segment
      if (sg.horiz && (cx - w / 2 < Math.min(sg.x0, sg.x1) + 6 || cx + w / 2 > Math.max(sg.x0, sg.x1) - 6) && sg.len > w + 14) continue;
      if (!sg.horiz && (cy < Math.min(sg.y0, sg.y1) + 10 || cy > Math.max(sg.y0, sg.y1) - 10)) continue;
      const box = { x: cx - w / 2 - 3, y: cy - 9, w: w + 6, h: 18 };
      if (!overlap(box, placed)) { placed.push(box); return { cx, cy, w, text }; }
    }
  }
  // fall back: middle of the longest, even if it overlaps
  const sg = segs[0], cx = (sg.x0 + sg.x1) / 2, cy = (sg.y0 + sg.y1) / 2;
  placed.push({ x: cx - w / 2 - 3, y: cy - 9, w: w + 6, h: 18 });
  return { cx, cy, w, text };
}

function drawEdges(edges, parent, placed) {
  for (const e of edges) {
    const g = S('g', { class: `edge p-${e.p || 'call'}`, 'data-n': e.n || null, 'data-a': e.a, 'data-b': e.b, 'data-sys': (e.sys || []).join(' ') || null });
    const d = e.pts.map((p, i) => (i ? 'L' : 'M') + p[0] + ',' + p[1]).join(' ');
    g.append(S('path', { class: 'hit', d }), S('path', { class: 'ln', d }));
    const last = e.pts.length - 1;
    g.append(S('path', { class: 'head', d: headPath(e.pts[last - 1], e.pts[last]) }));
    if (e.d === '<->') g.append(S('path', { class: 'head', d: headPath(e.pts[1], e.pts[0]) }));
    const L = placeLabel(e, placed);
    if (L) {
      const lg = S('g', { class: 'lg' });
      lg.append(S('rect', { class: 'lbg', x: L.cx - L.w / 2 - 4, y: L.cy - 9, width: L.w + 8, height: 18, rx: 4 }));
      let tx = L.cx;
      if (e.n) {
        const bx = L.cx - L.w / 2 + 9;
        lg.append(S('circle', { class: 'bd', cx: bx, cy: L.cy, r: 9 }), S('text', { class: 'bn', x: bx, y: L.cy + 3.6 }, String(e.n)));
        tx = L.cx - L.w / 2 + 22 + (L.text ? 4 : 0) + tw(L.text, FONT.lab) / 2;
      }
      if (L.text) lg.append(S('text', { class: 'lab', x: tx, y: L.cy + 3.6, 'text-anchor': 'middle' }, L.text));
      g.append(lg);
    }
    parent.append(g);
  }
}

function drawNode(n, parent) {
  const cls = ['node', 'k-' + n.kind];
  if (n.ghost) cls.push('ghost'); if (n.go) cls.push('go');
  const g = S('g', { class: cls.join(' '), 'data-id': n.id, tabindex: n.go ? 0 : null, role: n.go ? 'link' : null, 'aria-label': n.go ? 'Open ' + n.t : null });
  g.append(S('rect', { class: 'body', x: n.x, y: n.y, width: n.w, height: n.h, rx: 8 }));
  g.append(S('rect', { class: 'stripe', x: n.x, y: n.y + 9, width: 4, height: n.h - 18, rx: 2 }));
  g.append(S('text', { class: 't' + (n.m ? ' m' : ''), x: n.x + 16, y: n.y + 22 }, n.t));
  n.lines.forEach((l, i) => g.append(S('text', { class: 's', x: n.x + 16, y: n.y + 39 + i * 15 }, l)));
  parent.append(g);
  return g;
}

function drawGroups(groups, parent) {
  for (const gr of groups) {
    const g = S('g', { class: 'group' });
    g.append(S('rect', { x: gr.x, y: gr.y, width: gr.w, height: gr.h, rx: 14 }));
    g.append(S('text', { x: gr.x + 14, y: gr.y + 18 }, gr.t));
    parent.append(g);
  }
}

/* Build the isolation SVG for one system. Returns {svg, edges}. */
function buildIsolation(sysId, go) {
  const d = DIA[sysId];
  const L = layoutDiagram(d);
  for (const g of (d.ghosts || []).concat(d.nodes.filter(n => n.ghost))) g.go = !!SYSBY[g.sys] && g.sys !== sysId;
  const rects = L.rects;
  const edges = [];
  for (const [a, b, lab, p] of d.edges || []) edges.push({ a, b, lab: lab || '', p });
  for (const x of d.x || []) edges.push({ a: x.a, b: x.b, lab: x.lab || '', n: x.n || null, p: x.p || (COR_BY[x.cor && x.cor[0]] || {}).p || 'http', d: x.d, cor: x.cor });
  // a ghost→node edge keeps its direction as written
  routeEdges(edges, rects, L.W, L.H);
  const svg = S('svg', { viewBox: `0 0 ${L.W} ${L.H}`, role: 'img', 'aria-label': (SYSBY[sysId].name) + ' in isolation' });
  drawGroups(L.groups, svg);
  const gEdges = S('g', { class: 'edges' });
  const placed = [];
  Object.values(rects).filter(r => !r.hidden).forEach(r => placed.push({ x: r.x - 2, y: r.y - 2, w: r.w + 4, h: r.h + 4 }));
  L.groups.forEach(gr => placed.push({ x: gr.x + 8, y: gr.y + 2, w: tw(gr.t.toUpperCase(), '500 10.5px "IBM Plex Mono", monospace') * 1.12 + 8, h: 22 }));
  drawEdges(edges, gEdges, placed);
  svg.append(gEdges);
  const gNodes = S('g', { class: 'nodes' });
  for (const n of Object.values(rects).filter(r => !r.hidden)) {
    const el = drawNode(n, gNodes);
    if (n.ghost && n.go) {
      const open = () => go(n.sys);
      el.addEventListener('click', open);
      el.addEventListener('keydown', ev => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); open(); } });
    }
  }
  svg.append(gNodes);
  return { svg, edges };
}
