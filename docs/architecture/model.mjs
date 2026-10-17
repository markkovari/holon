/* Pure functions over the map, shared by the generator (Node) and the page (browser).
 *
 * No DOM, no files. build.mjs imports this file, and inlines it into index.html with the
 * `export` keywords removed, so keep it to top-level `export function` and `export const`. */

/** The correspondences by id. */
export const corBy = COR => Object.fromEntries(COR.map(c => [c.id, c]));

/** GitHub's heading slug, so a link to a section lands on it: lowercase, keep letters, digits,
 *  spaces, hyphens and underscores, spaces to hyphens. The docs link test uses the same rule. */
export function slug(heading) {
  return heading.trim().toLowerCase().replace(/[^a-z0-9 _-]/g, '').trim().replace(/ /g, '-');
}

/** `a/{b,c}-{d,e}.rs` is every path that shorthand stands for. */
export function braceExpand(p) {
  const m = p.match(/\{([^{}]*)\}/);
  if (!m) return [p];
  const before = p.slice(0, m.index), after = p.slice(m.index + m[0].length);
  return m[1].split(',').flatMap(alt => braceExpand(before + alt + after));
}

/** The rows of one system's table of correspondences. A row the drawing shows carries the numbers
 *  on its lines; the other correspondences that touch the system follow, unnumbered. `here` is this
 *  system's end of it, `other` the peer's, and `arrow` says which way it goes from here. */
export function isoRows(sysId, dia, COR) {
  const by = corBy(COR);
  const nums = new Map();
  for (const e of dia.x || []) {
    if (!e.n || !e.cor) continue;
    for (const cid of e.cor) {
      if (!nums.has(cid)) nums.set(cid, { ns: [], d: e.d });
      const r = nums.get(cid);
      if (!r.ns.includes(e.n)) r.ns.push(e.n);
    }
  }
  const drawn = [...nums.entries()].map(([cid, v]) => ({ c: by[cid], ns: v.ns, d: v.d })).sort((a, b) => a.ns[0] - b.ns[0]);
  const rest = COR.filter(c => (c.a[0] === sysId || c.b[0] === sysId) && !nums.has(c.id)).map(c => ({ c, ns: [], d: undefined }));
  return [...drawn, ...rest].map(r => {
    const mine = r.c.a[0] === sysId;
    return { c: r.c, ns: r.ns, d: r.d, mine, here: mine ? r.c.a : r.c.b, other: mine ? r.c.b : r.c.a, arrow: r.d === '<->' ? '⇄' : (mine ? '→' : '←') };
  });
}
