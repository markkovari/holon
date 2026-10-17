#!/usr/bin/env node
/* Builds docs/ARCHITECTURE.md and docs/architecture/index.html from map.mjs, and checks the map.
 *
 *   node docs/architecture/build.mjs                    write both
 *   node docs/architecture/build.mjs --check            exit 1 if either is stale, or the map names something that is not there
 *   node docs/architecture/build.mjs --fragment <file>  write the page without a document wrapper, for a host that supplies one
 *
 * Node only, no dependencies. The docs test (reconciler/tests/docs.rs) runs --check, so a stale
 * output or a renamed file is a failing test instead of a quietly wrong map.
 */

import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import * as M from './map.mjs';
import { braceExpand, corBy, isoRows, slug } from './model.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, '..', '..');
const MD_PATH = join(ROOT, 'docs', 'ARCHITECTURE.md');
const HTML_PATH = join(HERE, 'index.html');

const read = name => readFileSync(join(HERE, name), 'utf8');
const COR_BY = corBy(M.COR);
const SYS_BY = Object.fromEntries(M.SYS.map(s => [s.id, s]));
const SUB_BY = Object.fromEntries(M.SUB.map(s => [s.id, s]));
const ACTOR_BY = Object.fromEntries(M.ACTORS.map(a => [a.id, a]));

/* ------------------------------------------------------------------ the check */

/** ADR number -> file name, read from docs/adr so a link can only name an ADR that exists. */
const ADR = Object.fromEntries(
  readdirSync(join(ROOT, 'docs', 'adr'))
    .filter(f => /^\d{4}-.*\.md$/.test(f))
    .sort()
    .map(f => [f.slice(0, 4), f])
);

/** What a path in the map stands for: one file or directory, or a wildcard that matches something. */
function pathExists(p) {
  const rel = p.replace(/\/$/, '');
  if (!rel.includes('*')) return existsSync(join(ROOT, rel));
  const dir = dirname(rel);
  const re = new RegExp('^' + rel.slice(dir.length + 1).replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*') + '$');
  return existsSync(join(ROOT, dir)) && readdirSync(join(ROOT, dir)).some(f => re.test(f));
}

function validate() {
  const bad = [];
  const problem = msg => bad.push(msg);
  const sysIds = new Set(M.SYS.map(s => s.id));
  const ends = new Set([...sysIds, ...M.SUB.map(s => s.id), ...M.ACTORS.map(a => a.id)]);
  const kinds = new Set(M.KINDS.map(k => k[0]));
  const protos = new Set(Object.keys(M.PROTO));
  const unique = (what, ids) => { const seen = new Set(); for (const i of ids) { if (seen.has(i)) problem(`${what}: ${i} appears twice`); seen.add(i); } };

  unique('SYS', M.SYS.map(s => s.id));
  unique('COR', M.COR.map(c => c.id));
  unique('SUB', M.SUB.map(s => s.id));

  for (const s of M.SYS) {
    if (!M.DIA[s.id]) problem(`${s.id} has no DIA entry`);
    if (!M.OV_POS[s.id]) problem(`${s.id} has no OV_POS entry`);
    for (const u of s.uses) if (!SUB_BY[u]) problem(`${s.id} uses ${u}, which is not in SUB`);
    for (const [k] of s.parts) if (!kinds.has(k)) problem(`${s.id}: part kind ${k} is not in KINDS`);
    for (const pattern of s.code) for (const p of braceExpand(pattern)) if (!pathExists(p)) problem(`${s.id}: ${p} does not exist`);
  }
  for (const e of M.OV_EDGES) {
    for (const end of [e.a, e.b]) if (!sysIds.has(end)) problem(`OV_EDGES: ${end} is not a system`);
    if (!protos.has(e.p)) problem(`OV_EDGES: ${e.p} is not in PROTO`);
  }
  for (const c of M.COR) {
    for (const [end] of [c.a, c.b]) if (!ends.has(end)) problem(`${c.id}: ${end} is not a system, state or client`);
    if (!protos.has(c.p)) problem(`${c.id}: ${c.p} is not in PROTO`);
    for (const n of c.adr.match(/\d{4}/g) || []) if (!ADR[n]) problem(`${c.id}: ADR ${n} does not exist`);
  }
  // Every ADR the prose names.
  const prose = JSON.stringify([M.SYS, M.COR, M.DIA, M.ZONES, M.RULE, M.CREDS, M.GUEST_NAMES]);
  for (const m of prose.matchAll(/ADR-(\d{4})/g)) if (!ADR[m[1]]) problem(`the map names ADR-${m[1]}, which does not exist`);
  if (!ADR[M.META.revisedAfter]) problem(`META.revisedAfter: ADR ${M.META.revisedAfter} does not exist`);

  for (const [sys, d] of Object.entries(M.DIA)) {
    if (!sysIds.has(sys)) problem(`DIA.${sys} is not a system`);
    const ids = [...d.nodes.map(n => n.id), ...(d.ghosts || []).map(g => g.id)];
    unique(`DIA.${sys}`, ids);
    const known = new Set([...ids, ...(d.groups || []).map(g => 'grp:' + g.id)]);
    for (const n of d.nodes) if (!kinds.has(n.k)) problem(`DIA.${sys}.${n.id}: kind ${n.k} is not in KINDS`);
    for (const g of d.ghosts || []) if (!ends.has(g.sys) && g.sys !== 'ext') problem(`DIA.${sys}.${g.id}: ${g.sys} is not a system, state or client`);
    for (const gr of d.groups || []) for (const id of gr.ids) if (!known.has(id)) problem(`DIA.${sys}: group ${gr.id} names ${id}`);
    for (const [a, b, , p] of d.edges || []) {
      for (const end of [a, b]) if (!known.has(end)) problem(`DIA.${sys}: edge ${a} -> ${b} names ${end}`);
      if (!protos.has(p)) problem(`DIA.${sys}: edge ${a} -> ${b} has line style ${p}`);
    }
    const numbers = [];
    for (const x of d.x || []) {
      for (const end of [x.a, x.b]) if (!known.has(end)) problem(`DIA.${sys}: correspondence ${x.a} -> ${x.b} names ${end}`);
      if (x.p && !protos.has(x.p)) problem(`DIA.${sys}: correspondence ${x.a} -> ${x.b} has line style ${x.p}`);
      if (x.n) numbers.push(x.n);
      for (const cid of x.cor || []) {
        const c = COR_BY[cid];
        if (!c) problem(`DIA.${sys}: ${cid} is not in COR`);
        else if (c.a[0] !== sys && c.b[0] !== sys) problem(`DIA.${sys}: ${cid} is not a correspondence of ${sys}`);
      }
    }
    unique(`DIA.${sys} line numbers`, numbers);
  }
  return bad;
}

/* ------------------------------------------------------------------ the page */

/** `const NAME = <json>;` for every export the page's scripts read, with `<` escaped so a string can never close the script. */
function prelude() {
  const names = ['META', 'KINDS', 'PROTO', 'SUB', 'ACTORS', 'SYS', 'OV_POS', 'OV_EDGES', 'COR', 'DIA', 'GUEST_NAMES', 'CREDS', 'ZONES', 'TENANTS', 'RULE'];
  return names.map(n => `const ${n} = ${JSON.stringify(M[n]).replace(/</g, '\\u003c')};`).join('\n');
}

function page({ fragment }) {
  const [head, body] = read('page.html').split('<!-- body -->\n');
  const model = read('model.mjs').replace(/^export /gm, '');
  const script = `<script>\n${prelude()}\n${model}\n${read('engine.js')}\n${read('views.js')}</script>\n`;
  if (fragment) {
    const nl = head.indexOf('\n') + 1;
    return head.slice(0, nl) + '<meta name="claude:viewport-fit-sides" content="cover">\n' + head.slice(nl) + body + script;
  }
  return `<!doctype html>
<!-- Generated by docs/architecture/build.mjs from map.mjs, page.html, model.mjs, engine.js and views.js. Do not edit this file: edit those and run the build again. -->
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
${head}</head>
<body>
${body}${script}</body>
</html>
`;
}

/* ------------------------------------------------------------------ markdown */

const adrFile = n => ADR[n];
const adrLink = n => `[${n}](adr/${adrFile(n)})`;
const adrCell = s => (s ? s.split(/\s*·\s*/).map(adrLink).join(' · ') : '');

/** Prose to markdown text: nothing in it may be read as markup or as an HTML tag. */
function text(s) {
  return String(s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/([*[\]|`])/g, '\\$1')
    .replace(/ADR-(\d{4})/g, (m, n) => `[ADR-${n}](adr/${adrFile(n)})`);
}
/** The same inside a table cell, where `|` ends the cell. */
const cell = s => text(s);

const sysLink = id => `[${SYS_BY[id].name}](#${slug(SYS_BY[id].name)})`;
const nameOf = id => (SYS_BY[id] ? SYS_BY[id].name : (SUB_BY[id] || ACTOR_BY[id] || { t: id }).t);
const peerCell = side => {
  const [id, part] = side;
  return `${SYS_BY[id] ? sysLink(id) : text(nameOf(id))}<br>${text(part)}`;
};

/* -------- mermaid */

const mq = s => String(s).replace(/&/g, '#amp;').replace(/"/g, '#quot;').replace(/</g, '#lt;').replace(/>/g, '#gt;');
const label = (title, lines) => `<b>${mq(title)}</b>${lines.map(l => '<br/>' + mq(l)).join('')}`;
const mid = id => 'n_' + id.replace(/[^A-Za-z0-9]/g, '_');
const gid = id => 'grp_' + id.replace(/[^A-Za-z0-9]/g, '_');
const refId = id => (id.startsWith('grp:') ? gid(id.slice(4)) : mid(id));

const CLASSES = `  classDef wasm fill:#e1f3ef,stroke:#0a6a5f,color:#10231f
  classDef native fill:#fbecdc,stroke:#954709,color:#2b1704
  classDef store fill:#eae7fa,stroke:#4a3fb0,color:#17123f
  classDef actor fill:#e4ebf7,stroke:#2a4977,color:#0f1d33
  classDef spec fill:#eef1f5,stroke:#465160,color:#1b2028
  classDef ext fill:#ffffff,stroke:#667180,color:#1b2028,stroke-dasharray:5 4
  classDef ghost fill:#ffffff,stroke:#667180,color:#1b2028,stroke-dasharray:4 3`;

/* Mermaid wraps a label at 200px, which breaks every line the page's wider boxes were written for. */
const WIDE = `%%{init: {"flowchart": {"wrappingWidth": 360}}}%%`;
const DASH = { nats: '7 4', art: '9 3 2 3' };

function linkStyles(edges) {
  const out = [];
  edges.forEach((e, i) => { if (DASH[e.p]) out.push(`  linkStyle ${i} stroke-dasharray:${DASH[e.p]}`); });
  return out;
}
const arrow = (lab, both) => (lab ? (both ? `<-- "${mq(lab)}" -->` : `-- "${mq(lab)}" -->`) : both ? '<-->' : '-->');

function mermaidOverview() {
  const out = ['flowchart LR', '  classDef sys fill:#ffffff,stroke:#465160,color:#11151b'];
  for (const s of M.SYS) out.push(`  ${mid(s.id)}["${label(s.name, [s.short])}"]:::sys`);
  for (const e of M.OV_EDGES) out.push(`  ${mid(e.a)} ${arrow(e.lab, e.d === '<->')} ${mid(e.b)}`);
  out.push(...linkStyles(M.OV_EDGES));
  return out.join('\n');
}

function mermaidSystem(sysId) {
  const d = M.DIA[sysId];
  const out = [WIDE, `flowchart ${d.dir || 'LR'}`, CLASSES];
  const grouped = new Set();
  const node = n => {
    const body = `"${label(n.t, n.s)}"`;
    const shape = n.ghost ? `(${'[' + body + ']'})` : n.k === 'store' ? `[(${body})]` : `[${body}]`;
    return `${mid(n.id)}${shape}:::${n.ghost ? 'ghost' : n.k}`;
  };
  const byGrid = (a, b) => a.r - b.r || a.c - b.c;
  for (const gr of d.groups || []) {
    out.push(`  subgraph ${gid(gr.id)}["${mq(gr.t)}"]`);
    for (const n of d.nodes.filter(x => gr.ids.includes(x.id)).sort(byGrid)) { out.push('    ' + node(n)); grouped.add(n.id); }
    out.push('  end');
  }
  for (const n of d.nodes.filter(x => !grouped.has(x.id)).sort(byGrid)) out.push('  ' + node(n));
  for (const g of d.ghosts || []) {
    out.push(`  ${mid(g.id)}(["${label(g.t, g.s)}"]):::ghost`);
  }
  const edges = [
    ...(d.edges || []).map(([a, b, lab, p]) => ({ a, b, lab, p })),
    ...(d.x || []).map(x => ({ a: x.a, b: x.b, lab: [x.n, x.lab].filter(Boolean).join(' · '), p: x.p || (COR_BY[(x.cor || [])[0]] || {}).p || 'http', d: x.d }))
  ];
  for (const e of edges) out.push(`  ${refId(e.a)} ${arrow(e.lab, e.d === '<->')} ${refId(e.b)}`);
  out.push(...linkStyles(edges));
  return out.join('\n');
}

function mermaidBoundaries() {
  const out = ['flowchart TB'];
  const pad = i => '  '.repeat(i + 1);
  M.ZONES.forEach((z, i) => {
    out.push(`${pad(i)}subgraph ${z.id}["${mq(z.t)}"]`);
    if (z.items.length) out.push(`${pad(i + 1)}${z.id}_items["${mq(z.items.join(' · '))}"]`);
    // the innermost zone holds the tenants
    if (i === M.ZONES.length - 1) M.TENANTS.names.forEach((name, k) => out.push(`${pad(i + 1)}${z.id}_t${k}["${label(name, ['component instances'])}"]`));
  });
  for (let i = M.ZONES.length - 1; i >= 0; i--) out.push(`${pad(i)}end`);
  return out.join('\n');
}

const fence = body => '```mermaid\n' + body + '\n```';

/* -------- sections */

function table(head, rows) {
  return [`| ${head.join(' | ')} |`, `|${head.map(() => ' --- ').join('|')}|`, ...rows.map(r => `| ${r.join(' | ')} |`)].join('\n');
}

function codeLinks(s) {
  const links = [];
  for (const pattern of s.code) {
    for (const p of braceExpand(pattern)) {
      const target = p.includes('*') ? dirname(p) + '/' : p;
      links.push(`[\`${p}\`](../${target})`);
    }
  }
  return links.join(' · ');
}

function systemSection(s) {
  const rows = isoRows(s.id, M.DIA[s.id], M.COR).map(r => [
    r.ns.join(', '),
    cell(r.here[1]),
    r.arrow,
    peerCell(r.other),
    cell(r.c.over + (r.c.internal ? ' · inside' : '')),
    cell(r.c.contract),
    adrCell(r.c.adr)
  ]);
  return [
    `### ${s.name}`,
    '',
    text(s.blurb),
    '',
    `**Reached through** ${text(s.entry)}.`,
    '',
    fence(mermaidSystem(s.id)),
    '',
    table(['', 'Here', '', 'Peer', 'Over', 'What crosses', 'Decided in'], rows),
    '',
    '**What keeps it apart**',
    '',
    ...s.boundary.map(b => `- ${text(b)}`),
    '',
    `**Where it lives** ${codeLinks(s)}`,
    ''
  ].join('\n');
}

function markdown() {
  const lines = [];
  const add = (...l) => lines.push(...l);
  add(
    '# Architecture',
    '',
    `*Last revised after ${adrLink(M.META.revisedAfter).replace('[', '[ADR-')}.* Generated from [\`architecture/map.mjs\`](architecture/map.mjs); do not edit this file, edit the map and run \`node docs/architecture/build.mjs\`.`,
    '',
    'A map of the whole repository: the systems it is made of, how each one stands on its own, and what each one says to the others. [`CURRENT.md`](CURRENT.md) goes deeper on the runtime and delivery half and carries the measurements; this page covers every system, including the ones added since (park, the code store, the agent services). Where a map and the code disagree, the code wins.',
    '',
    'There is an interactive version of the same map, with every system drawn alone and every correspondence numbered on its line: [`architecture/index.html`](architecture/index.html). Open it in a browser.',
    '',
    '## Reading the diagrams',
    '',
    'A box is coloured by what it is made of, so the line between sandboxed guest code and native processes shows everywhere.',
    '',
    table(['Box', 'Is'], [
      ['teal', 'a wasm component, a sandboxed guest'],
      ['orange', 'a native process'],
      ['violet', 'a store or a bus'],
      ['blue', 'a client or a person'],
      ['grey', 'a written spec, contract or check'],
      ['dashed', 'a peer outside the system being drawn, or outside this repository']
    ]),
    '',
    'A solid line is a request and a response over HTTP or gRPC, a dashed line is NATS, a dash-dot line is an artifact moving (an OCI push, a rendered file, a pull request). The arrow points from the one that starts it. On a system\'s own diagram a number on a line matches a row of the table under it.',
    '',
    '## The whole',
    '',
    fence(mermaidOverview()),
    '',
    'The primary correspondences only; [every correspondence](#every-correspondence) is listed below.',
    '',
    table(['System', 'What it is', 'Reached through'], M.SYS.map(s => [`**${sysLink(s.id)}**`, text(s.purpose), text(s.entry)])),
    '',
    '### State the systems sit on',
    '',
    table(['State', 'What lives there', 'Used by'], M.SUB.map(b => [
      `**${text(b.t)}**`,
      text(b.s.join(' ')),
      M.SYS.filter(s => s.uses.includes(b.id)).map(s => sysLink(s.id)).join(', ')
    ])),
    '',
    '### Who uses it',
    '',
    table(['Who', 'How', 'Systems'], M.ACTORS.map(a => [`**${text(a.t)}**`, text(a.s), a.touch.map(sysLink).join(', ')])),
    '',
    '## The systems',
    '',
    ...M.SYS.flatMap(s => [systemSection(s)]),
    '## Every correspondence',
    '',
    'One row for each thing one system says to another, with what crosses, over what, and the decision that made it so. A row marked *inside* is between two parts of one system.',
    '',
    table(['From', '', 'To', 'Over', 'What crosses', 'Decided in'], M.COR.map(c => [
      `${peerCell(c.a)}`, '\u2192', `${peerCell(c.b)}`,
      cell(c.over + (c.internal ? ' · inside' : '')), cell(c.contract), adrCell(c.adr)
    ])),
    '',
    '## Boundaries',
    '',
    `> **${text(M.RULE.t)}**`,
    '>',
    `> ${text(M.RULE.note)}`,
    '',
    'Each zone sits inside the one before it.',
    '',
    fence(mermaidBoundaries()),
    '',
    table(['Zone', 'What holds there', 'In it'], [
      ...M.ZONES.map(z => [`**${text(z.t)}**`, text(z.d), text(z.items.join(' · '))]),
      ['**A tenant**', text(M.TENANTS.text), text(M.TENANTS.names.join(' · '))]
    ]),
    '',
    '### What the guest names, and what the host makes of it',
    '',
    table(['The guest names', 'The host resolves it to', 'Decided in'], M.GUEST_NAMES.map(r => [text(r[0]), text(r[1]), adrCell(r[2])])),
    '',
    '### Who holds what',
    '',
    table(['Credential', 'Held by', 'Guards', 'Decided in'], M.CREDS.map(r => [`**${text(r[0])}**`, text(r[1]), text(r[2]), adrCell(r[3])])),
    '',
    '## Keeping this current',
    '',
    '`node docs/architecture/build.mjs` rewrites this page and `architecture/index.html` from [`architecture/map.mjs`](architecture/map.mjs). `--check` is what the docs test runs: it fails when either output is stale, and when the map names a path or an ADR that does not exist, so a rename shows up there instead of staying wrong here.',
    '',
    'The map holds no counts on purpose. How many components or interfaces there are is for `comp-capgraph` to say; a number copied into a page is a number somebody has to remember to edit.',
    ''
  );
  return lines.join('\n');
}

/* ------------------------------------------------------------------ main */

const args = process.argv.slice(2);
const problems = validate();
if (problems.length) {
  console.error(`architecture map: ${problems.length} problem(s)\n` + problems.map(p => '  ' + p).join('\n'));
  process.exit(1);
}

const fragmentAt = args.indexOf('--fragment');
if (fragmentAt >= 0) {
  const target = args[fragmentAt + 1];
  if (!target) { console.error('--fragment needs a file'); process.exit(2); }
  writeFileSync(target, page({ fragment: true }));
  console.log(`wrote ${target}`);
  process.exit(0);
}

const outputs = [[MD_PATH, markdown()], [HTML_PATH, page({ fragment: false })]];
if (args.includes('--check')) {
  const stale = outputs.filter(([p, body]) => !existsSync(p) || readFileSync(p, 'utf8') !== body).map(([p]) => p.slice(ROOT.length + 1));
  if (stale.length) {
    console.error(`architecture map: ${stale.join(' and ')} ${stale.length > 1 ? 'are' : 'is'} stale. Run: node docs/architecture/build.mjs`);
    process.exit(1);
  }
  console.log('architecture map: current, and every path and ADR it names exists');
} else {
  for (const [p, body] of outputs) { writeFileSync(p, body); console.log(`wrote ${p.slice(ROOT.length + 1)}`); }
}
