//! The maps beside the sessions: the components (what `comp-capgraph` projects into the store) and what
//! the goal runs left behind (the knowledge pool), both read from the one store (ADR-0091) and both drawn
//! as frames of items on a canvas that pans and zooms.
//!
//! A thing has one key on every tab (`component:rate-limiter`, `run:<id>`, `session:<root>`), so a
//! reference from one tab to another is that key, and the backlinks of a thing are everything that
//! names it. Pure, like the tree: rows in, frames, positions and references out, so every rule is a test.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What a reference is made of: the same key on every tab.
pub type Key = String;

pub fn component_key(name: &str) -> Key {
    format!("component:{name}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    /// What is happening: the goals given to agents, and the trees they started.
    Sessions,
    /// One thing at a time, and what it is joined to.
    Explore,
    /// The conversational agents: who each is, what it was asked, and where it is in Element.
    Agents,
    /// The whole system at once: every component, grouped, and how the groups use each other.
    Map,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An application: what a person reaches over HTTP.
    App,
    /// A capability component, that others import.
    Part,
    Probe,
    /// A goal run, one try at it that passed its gate, and one that did not.
    Run,
    Attempt,
    Missed,
    /// Something a run learned.
    Lesson,
    /// A goal given to an agent on the board, and everything it started.
    Session,
    /// A conversational agent: one you talk to, here or in Element.
    Agent,
}

/// What shows a thing works: the film of it working, the files its gate runs, a check it passed or
/// failed, the pull request it became.
#[derive(Clone, Debug, PartialEq)]
pub enum Proof {
    /// A GIF or picture of it working, by path from the repository root.
    Media(String),
    /// A file that proves or makes the proof, and what it is.
    File {
        path: String,
        what: String,
    },
    Link {
        url: String,
        what: String,
    },
    /// One of the gate's checks.
    Check {
        id: String,
        passed: bool,
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub key: Key,
    pub label: String,
    pub sub: String,
    pub kind: Kind,
    /// How much it matters: a part's consumers, an attempt's fitness.
    pub weight: u32,
    pub proof: Vec<Proof>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub key: Key,
    pub title: String,
    pub sub: String,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Map {
    pub frames: Vec<Frame>,
    /// `(from, to, why)`: a component's import of an interface another one exports.
    pub edges: Vec<(Key, Key, String)>,
}

impl Map {
    pub fn items(&self) -> impl Iterator<Item = (&Frame, &Item)> {
        self.frames.iter().flat_map(|f| f.items.iter().map(move |i| (f, i)))
    }

    /// The frame and the item a key stands for, the first place it is drawn.
    pub fn find(&self, key: &str) -> Option<(usize, usize)> {
        self.frames
            .iter()
            .enumerate()
            .find_map(|(f, fr)| fr.items.iter().position(|i| i.key == key).map(|i| (f, i)))
    }
}

/// The capability graph as the store holds it: every built component, which interfaces each imports
/// and exports, and which components each application is composed of.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogRows {
    pub artifacts: Vec<String>,
    /// `(component, interface)`.
    pub imports: Vec<(String, String)>,
    pub exports: Vec<(String, String)>,
    /// `(app, its root component)`.
    pub apps: Vec<(String, String)>,
    /// `(app, a component it is composed of)`.
    pub carries: Vec<(String, String)>,
}

/// `records:store/store@0.2.0` as `records:store/store`.
fn short(iface: &str) -> &str {
    iface.split('@').next().unwrap_or(iface)
}

/// The components as a few frames by what they are to the applications: the applications themselves,
/// the parts two or more of them are made of, the parts only one uses, the probes, and what nothing
/// carries. Each is listed once; what an application is made of is its edges, drawn when it is selected.
pub fn components(rows: &CatalogRows) -> Map {
    let mut exporters: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut first_export: HashMap<&str, &str> = HashMap::new();
    for (c, i) in &rows.exports {
        exporters.entry(i.as_str()).or_default().push(c.as_str());
        if !i.starts_with("wasi:") {
            first_export.entry(c.as_str()).or_insert(short(i));
        }
    }
    let mut edges = Vec::new();
    let mut joined: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut consumers: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for (c, i) in &rows.imports {
        for p in exporters.get(i.as_str()).into_iter().flatten().filter(|p| **p != c.as_str()) {
            consumers.entry(p).or_default().insert(c.as_str());
            joined.insert((c.as_str(), p));
            edges.push((component_key(c), component_key(p), short(i).to_string()));
        }
    }
    // root component -> application
    let app_of: BTreeMap<&str, &str> =
        rows.apps.iter().map(|(a, r)| (r.as_str(), a.as_str())).collect();
    let root_of: HashMap<&str, &str> =
        rows.apps.iter().map(|(a, r)| (a.as_str(), r.as_str())).collect();
    let mut carried_by: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut parts: HashMap<&str, usize> = HashMap::new();
    for (a, p) in &rows.carries {
        if app_of.contains_key(p.as_str()) {
            continue; // an application's own component is not a part of it
        }
        carried_by.entry(p.as_str()).or_default().insert(a.as_str());
        *parts.entry(a.as_str()).or_default() += 1;
        // what an application is made of, where its imports have not said so already
        if let Some(r) = root_of.get(a.as_str()) {
            if joined.insert((r, p.as_str())) {
                edges.push((component_key(r), component_key(p), "composes".to_string()));
            }
        }
    }
    let item = |name: &str| match app_of.get(name) {
        Some(app) => {
            let n = parts.get(app).copied().unwrap_or(0);
            Item {
                key: component_key(name),
                label: app.to_string(),
                sub: if *app == name { "an application".into() } else { name.to_string() },
                kind: Kind::App,
                weight: n as u32,
                proof: Vec::new(),
            }
        }
        None => Item {
            key: component_key(name),
            label: name.to_string(),
            sub: first_export.get(name).map_or_else(String::new, |s| s.to_string()),
            kind: if name.ends_with("-probe") { Kind::Probe } else { Kind::Part },
            weight: consumers.get(name).map_or(0, |c| c.len() as u32),
            proof: Vec::new(),
        },
    };
    let by_weight = |v: &mut Vec<Item>| {
        v.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.label.cmp(&b.label)));
    };

    let (mut apps, mut shared, mut single, mut probes, mut loose) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let names: BTreeSet<&str> = rows.artifacts.iter().map(String::as_str).collect();
    for name in names.iter().copied() {
        let carriers = carried_by.get(name).map_or(0, |s| s.len());
        if app_of.contains_key(name) {
            apps.push(item(name));
        } else if carriers >= 2 {
            shared.push(item(name));
        } else if carriers == 1 {
            let app = carried_by[name].iter().next().copied().unwrap_or("");
            single.push((app, item(name)));
        } else if name.ends_with("-probe") {
            probes.push(item(name));
        } else {
            loose.push(item(name));
        }
    }
    by_weight(&mut apps);
    by_weight(&mut shared);
    by_weight(&mut loose);
    probes.sort_by(|a, b| a.label.cmp(&b.label));
    // a part only one application uses sits beside the others of that application
    single.sort_by(|a, b| a.0.cmp(b.0).then_with(|| b.1.weight.cmp(&a.1.weight)));
    let single: Vec<Item> = single.into_iter().map(|(_, i)| i).collect();

    let n = |v: &Vec<Item>| v.len();
    let frames = [
        (
            "frame:apps",
            "Applications",
            format!("{} things a person reaches over HTTP", n(&apps)),
            apps,
        ),
        (
            "frame:shared",
            "Shared",
            format!("{} parts two applications or more are made of", n(&shared)),
            shared,
        ),
        (
            "frame:single",
            "Used by one application",
            format!("{} parts only one application carries", n(&single)),
            single,
        ),
        ("frame:probes", "Probes", "components that check a host can do something".into(), probes),
        ("frame:loose", "In no application", "built, and carried by nothing yet".into(), loose),
    ]
    .into_iter()
    .filter(|f| !f.3.is_empty())
    .map(|(key, title, sub, items)| Frame { key: key.into(), title: title.into(), sub, items })
    .collect();
    Map { frames, edges }
}

/// The goal runs as the store holds them (ADR-0092): each run, its attempts with their fitness and the
/// gate's verdict, and the capabilities it left the pool able to do.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KnowledgeRows {
    pub runs: Vec<RunRow>,
    pub attempts: Vec<AttemptRow>,
    /// `(component, the run that added it)`.
    pub capabilities: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunRow {
    pub id: String,
    pub goal: String,
    /// `merged`, `exhausted`, `dry-run`, `failed`, or empty while it runs.
    pub outcome: String,
    pub winner: String,
    pub url: String,
    pub branches: u32,
    pub started: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttemptRow {
    pub id: String,
    pub run: String,
    pub branch: String,
    pub round: u32,
    /// `passed` (every required check did) or `failed`.
    pub outcome: String,
    /// The fitness: the share of the checks' weight that passed, out of 1000 (ADR-0081).
    pub score: u32,
    pub tokens: u64,
    pub paths: Vec<String>,
    /// The checks that failed, as the gate said them.
    pub failing: Vec<(String, String)>,
}

/// The goal runs as frames, newest first: what was asked, and in it every attempt with its fitness and
/// the components the run added. An edge runs from the attempt that wrote a component to it.
pub fn knowledge(rows: &KnowledgeRows) -> Map {
    let mut runs: Vec<&RunRow> = rows.runs.iter().collect();
    runs.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    let mut frames = Vec::new();
    let mut edges = Vec::new();
    for r in runs {
        let mut attempts: Vec<&AttemptRow> =
            rows.attempts.iter().filter(|a| a.run == r.id).collect();
        attempts.sort_by(|a, b| (a.round, &a.branch).cmp(&(b.round, &b.branch)));
        let mut items: Vec<Item> = attempts
            .iter()
            .map(|a| {
                let won = a.branch == r.winner;
                let mut proof: Vec<Proof> = a
                    .failing
                    .iter()
                    .map(|(id, detail)| Proof::Check {
                        id: id.clone(),
                        passed: false,
                        detail: detail.clone(),
                    })
                    .collect();
                if a.outcome == "passed" {
                    proof.insert(
                        0,
                        Proof::Check {
                            id: "every required check".into(),
                            passed: true,
                            detail: String::new(),
                        },
                    );
                }
                if won && !r.url.is_empty() {
                    proof.push(Proof::Link {
                        url: r.url.clone(),
                        what: "the pull request it became".into(),
                    });
                }
                Item {
                    key: format!("attempt:{}", a.id),
                    label: format!("round {} · {}", a.round, a.branch),
                    sub: format!(
                        "{}{} · fitness {}",
                        if won { "won · " } else { "" },
                        a.outcome,
                        a.score
                    ),
                    kind: if a.outcome == "passed" { Kind::Attempt } else { Kind::Missed },
                    weight: a.score,
                    proof,
                }
            })
            .collect();
        for (name, by) in rows.capabilities.iter().filter(|(_, by)| *by == r.id) {
            let _ = by;
            let key = component_key(name);
            for a in &attempts {
                if a.paths.iter().any(|p| p.starts_with(&format!("components/{name}/"))) {
                    edges.push((format!("attempt:{}", a.id), key.clone(), "wrote".to_string()));
                }
            }
            items.push(Item {
                key,
                label: name.clone(),
                sub: "a capability this run added".into(),
                kind: Kind::Part,
                weight: 0,
                proof: Vec::new(),
            });
        }
        let best = attempts.iter().map(|a| a.score).max().unwrap_or(0);
        frames.push(Frame {
            key: format!("run:{}", r.id),
            title: if r.goal.is_empty() { r.id.clone() } else { r.goal.clone() },
            sub: format!(
                "{} · {} attempt{} · best fitness {best}",
                if r.outcome.is_empty() { "running" } else { r.outcome.as_str() },
                attempts.len(),
                if attempts.len() == 1 { "" } else { "s" }
            ),
            items,
        });
    }
    Map { frames, edges }
}

// -- where things go ----------------------------------------------------------------------------

pub const ITEM_W: f32 = 200.0;
pub const ITEM_H: f32 = 50.0;
pub const ITEM_GAP: f32 = 12.0;
pub const PAD: f32 = 22.0;
pub const HEAD: f32 = 68.0;
pub const FRAME_GAP: f32 = 140.0;
/// The shape the whole is laid out to: about a screen's.
const ASPECT: f32 = 1.6;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }

    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// The smallest rectangle holding both.
    pub fn union(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect {
            x,
            y,
            w: (self.x + self.w).max(o.x + o.w) - x,
            h: (self.y + self.h).max(o.y + o.h) - y,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub frame: Rect,
    pub items: Vec<Rect>,
}

/// How many columns a frame of `n` items has: about square on screen and never more than eight, or,
/// for a frame in a column of them, taller than wide and never more than six.
fn grid_columns(n: usize, tall: bool) -> usize {
    if tall {
        ((n as f32 * 0.3).sqrt().ceil() as usize).clamp(1, 6)
    } else {
        ((n as f32 * 0.6).sqrt().ceil() as usize).clamp(1, 8)
    }
}

/// Between two columns of frames: room for the connections to bend and for their counts.
pub const COLUMN_GAP: f32 = 380.0;

/// Where everything goes: each frame a grid of its items; frames joined by connections in columns, so
/// that what uses runs to what is used left to right, and frames that are not in shelves.
pub fn layout(map: &Map) -> Vec<Placed> {
    let (between, _) = bundles(map);
    if between.is_empty() {
        return in_shelves(map);
    }
    let cols = columns(map.frames.len(), &between);
    let grids = shelves(map, f32::MAX, true);
    let ncols = cols.iter().max().map_or(1, |c| c + 1);
    let width: Vec<f32> = (0..ncols)
        .map(|c| {
            (0..grids.len()).filter(|f| cols[*f] == c).map(|f| grids[f].frame.w).fold(0.0, f32::max)
        })
        .collect();
    let height: Vec<f32> = (0..ncols)
        .map(|c| {
            let hs: Vec<f32> =
                (0..grids.len()).filter(|f| cols[*f] == c).map(|f| grids[f].frame.h).collect();
            hs.iter().sum::<f32>() + hs.len().saturating_sub(1) as f32 * FRAME_GAP
        })
        .collect();
    let tallest = height.iter().copied().fold(0.0, f32::max);
    let mut x0 = vec![0.0; ncols];
    for c in 1..ncols {
        x0[c] = x0[c - 1] + width[c - 1] + COLUMN_GAP;
    }
    // each column centred against the tallest, its frames in the map's order
    let mut y = (0..ncols).map(|c| (tallest - height[c]) / 2.0).collect::<Vec<f32>>();
    grids
        .into_iter()
        .enumerate()
        .map(|(f, g)| {
            let c = cols[f];
            let (dx, dy) = (x0[c] - g.frame.x, y[c] - g.frame.y);
            y[c] += g.frame.h + FRAME_GAP;
            let shift = |r: Rect| Rect { x: r.x + dx, y: r.y + dy, ..r };
            Placed { frame: shift(g.frame), items: g.items.into_iter().map(shift).collect() }
        })
        .collect()
}

/// Each frame's column, in three tiers: what only uses others on the left, what is only used on the
/// right, and what does both between; a frame no connection reaches goes last. Of two frames that use
/// each other only the heavier way counts. Empty tiers close up.
pub fn columns(n: usize, between: &[(usize, usize, usize)]) -> Vec<usize> {
    let weight =
        |a: usize, b: usize| between.iter().find(|e| e.0 == a && e.1 == b).map_or(0, |e| e.2);
    let edges: Vec<(usize, usize)> = between
        .iter()
        .filter(|&&(a, b, w)| a != b && (w > weight(b, a) || (w == weight(b, a) && a < b)))
        .map(|&(a, b, _)| (a, b))
        .collect();
    let tier: Vec<usize> = (0..n)
        .map(|f| {
            let (into, out) = (edges.iter().any(|e| e.1 == f), edges.iter().any(|e| e.0 == f));
            match (into, out) {
                (false, true) => 0,
                (true, true) => 1,
                (true, false) => 2,
                (false, false) => 3,
            }
        })
        .collect();
    let used: Vec<usize> = (0..4).filter(|t| tier.contains(t)).collect();
    tier.iter().map(|t| used.iter().position(|u| u == t).unwrap_or(0)).collect()
}

/// The frames in shelves in the map's own order, as wide as makes the whole closest to a screen's shape.
fn in_shelves(map: &Map) -> Vec<Placed> {
    let widths: Vec<f32> = map.frames.iter().map(|f| frame_w(f.items.len())).collect();
    let widest = widths.iter().copied().fold(0.0, f32::max);
    // every width at which a shelf can end, from one frame to all of them
    let mut candidates: Vec<f32> = (1..=widths.len())
        .flat_map(|k| (0..=widths.len() - k).map(move |i| (i, k)))
        .map(|(i, k)| widths[i..i + k].iter().sum::<f32>() + (k - 1) as f32 * FRAME_GAP)
        .filter(|w| *w >= widest)
        .collect();
    candidates.sort_by(f32::total_cmp);
    candidates.dedup();
    let off = |p: &[Placed]| {
        let b = bounds(p);
        if b.h <= 0.0 {
            f32::MAX
        } else {
            (b.w / b.h / ASPECT).ln().abs()
        }
    };
    candidates
        .into_iter()
        .map(|w| shelves(map, w, false))
        .min_by(|a, b| off(a).total_cmp(&off(b)))
        .unwrap_or_default()
}

fn frame_w(n: usize) -> f32 {
    let cols = grid_columns(n, false);
    2.0 * PAD + cols as f32 * ITEM_W + (cols - 1) as f32 * ITEM_GAP
}

/// The frames in shelves no wider than `shelf_w`, in the map's order.
fn shelves(map: &Map, shelf_w: f32, tall: bool) -> Vec<Placed> {
    let mut out = Vec::with_capacity(map.frames.len());
    let (mut x, mut y, mut shelf_h) = (0.0f32, 0.0f32, 0.0f32);
    for f in &map.frames {
        let cols = grid_columns(f.items.len(), tall);
        let rows = f.items.len().div_ceil(cols).max(1);
        let w = 2.0 * PAD + cols as f32 * ITEM_W + (cols - 1) as f32 * ITEM_GAP;
        let h = HEAD + PAD + rows as f32 * ITEM_H + (rows - 1) as f32 * ITEM_GAP;
        if x > 0.0 && x + w > shelf_w + 0.5 {
            (x, y, shelf_h) = (0.0, y + shelf_h + FRAME_GAP, 0.0);
        }
        let frame = Rect { x, y, w, h };
        let items = (0..f.items.len())
            .map(|i| Rect {
                x: x + PAD + (i % cols) as f32 * (ITEM_W + ITEM_GAP),
                y: y + HEAD + (i / cols) as f32 * (ITEM_H + ITEM_GAP),
                w: ITEM_W,
                h: ITEM_H,
            })
            .collect();
        out.push(Placed { frame, items });
        x += w + FRAME_GAP;
        shelf_h = shelf_h.max(h);
    }
    out
}

/// Everything laid out, as one rectangle: what "fit" fits.
pub fn bounds(placed: &[Placed]) -> Rect {
    placed.iter().map(|p| p.frame).reduce(|a, b| a.union(&b)).unwrap_or_default()
}

/// The connections between frames, counted: `(from, to, how many)` for each pair of different frames
/// with any, and for each frame how many run inside it. What the map draws when it is too far away for
/// its items: a few thick lines that say where the weight is, not eight hundred thin ones.
pub fn bundles(map: &Map) -> (Vec<(usize, usize, usize)>, Vec<usize>) {
    let mut frame_of: HashMap<&str, usize> = HashMap::new();
    for (f, fr) in map.frames.iter().enumerate() {
        for i in &fr.items {
            frame_of.entry(i.key.as_str()).or_insert(f);
        }
    }
    let mut between: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    let mut within = vec![0; map.frames.len()];
    for (a, b, _) in &map.edges {
        let (Some(&fa), Some(&fb)) = (frame_of.get(a.as_str()), frame_of.get(b.as_str())) else {
            continue;
        };
        if fa == fb {
            within[fa] += 1;
        } else {
            *between.entry((fa, fb)).or_default() += 1;
        }
    }
    (between.into_iter().map(|((a, b), n)| (a, b, n)).collect(), within)
}

/// Where a line from the middle of `r` towards `(x, y)` leaves it.
pub fn exit(r: &Rect, x: f32, y: f32) -> (f32, f32) {
    let (cx, cy) = r.centre();
    let (dx, dy) = (x - cx, y - cy);
    if dx == 0.0 && dy == 0.0 {
        return (cx, cy);
    }
    let t = (r.w / 2.0 / dx.abs().max(1e-6)).min(r.h / 2.0 / dy.abs().max(1e-6));
    (cx + dx * t, cy + dy * t)
}

// -- references ---------------------------------------------------------------------------------

/// Another place a thing is named, and what to call that place.
#[derive(Clone, Debug, PartialEq)]
pub struct Ref {
    pub tab: Tab,
    /// What to select there: a session's root, or a map's item key.
    pub key: Key,
    /// The frame it is in, where that matters (a component under one run or another).
    pub frame: Option<Key>,
    pub label: String,
}

/// The components a text names: a path through `components/<name>/`, or a name that is a component's,
/// as a whole word (`rate-limiter`, not `limiter`).
pub fn mentions(text: &str, names: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, _) in text.match_indices("components/") {
        let rest = &text[i + "components/".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if names.contains(&name) {
            out.insert(name);
        }
    }
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        if word.contains('-') && names.contains(word) {
            out.insert(word.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> CatalogRows {
        let s =
            |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        CatalogRows {
            artifacts: ["shop", "blog", "store", "auth", "cart", "loose-thing", "net-probe"]
                .map(String::from)
                .into(),
            exports: s(&[
                ("store", "records:store/store@0.2.0"),
                ("auth", "auth:identity/session@0.1.0"),
                ("cart", "shop:cart/cart@0.1.0"),
                ("shop", "wasi:http/incoming-handler@0.2.0"),
                ("blog", "wasi:http/incoming-handler@0.2.0"),
            ]),
            imports: s(&[
                ("shop", "records:store/store@0.2.0"),
                ("shop", "shop:cart/cart@0.1.0"),
                ("cart", "records:store/store@0.2.0"),
                ("blog", "records:store/store@0.2.0"),
                ("blog", "auth:identity/session@0.1.0"),
            ]),
            apps: s(&[("shop", "shop"), ("blog", "blog")]),
            carries: s(&[("shop", "store"), ("shop", "cart"), ("blog", "store"), ("blog", "auth")]),
        }
    }

    #[test]
    fn the_components_are_framed_by_what_they_are_to_the_applications() {
        let m = components(&rows());
        let titles: Vec<&str> = m.frames.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Applications", "Shared", "Used by one application", "Probes", "In no application"]
        );
        let labels =
            |i: usize| m.frames[i].items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels(0), ["blog", "shop"]);
        assert_eq!(labels(1), ["store"]);
        assert_eq!(labels(2), ["auth", "cart"], "each beside the others of its application");
        assert_eq!(labels(3), ["net-probe"]);
        assert_eq!(labels(4), ["loose-thing"]);
        let store = &m.frames[1].items[0];
        assert_eq!(
            (store.weight, store.sub.as_str(), store.kind),
            (3, "records:store/store", Kind::Part)
        );
        let shop = &m.frames[0].items[1];
        assert_eq!((shop.kind, shop.weight), (Kind::App, 2), "an application counts its parts");
        // every component is drawn exactly once
        assert_eq!(m.items().count(), 7);
    }

    #[test]
    fn what_an_application_is_made_of_is_an_edge_even_when_it_imports_nothing_of_it() {
        let mut r = rows();
        r.carries.push(("shop".into(), "loose-thing".into()));
        let m = components(&r);
        assert!(m.edges.contains(&(
            component_key("shop"),
            component_key("loose-thing"),
            "composes".into()
        )));
        // and once only, where an import already joins them
        assert_eq!(
            m.edges
                .iter()
                .filter(|e| e.0 == component_key("shop") && e.1 == component_key("cart"))
                .count(),
            1
        );
    }

    #[test]
    fn an_edge_runs_from_who_imports_to_who_exports() {
        let m = components(&rows());
        assert!(m.edges.contains(&(
            component_key("shop"),
            component_key("cart"),
            "shop:cart/cart".into()
        )));
        assert_eq!(m.edges.iter().filter(|e| e.1 == component_key("store")).count(), 3);
    }

    #[test]
    fn frames_never_overlap_and_hold_their_items() {
        let mut m = components(&rows());
        // and a big one, to force a second shelf
        m.frames.push(Frame {
            key: "big".into(),
            title: "big".into(),
            sub: String::new(),
            items: (0..60)
                .map(|i| Item {
                    key: format!("k{i}"),
                    label: String::new(),
                    sub: String::new(),
                    kind: Kind::Part,
                    weight: 0,
                    proof: vec![],
                })
                .collect(),
        });
        let placed = in_shelves(&m);
        assert!(!layout(&m)
            .iter()
            .enumerate()
            .any(|(i, a)| layout(&m)[i + 1..].iter().any(|b| a.frame.overlaps(&b.frame))));
        for (i, a) in placed.iter().enumerate() {
            for r in &a.items {
                assert!(
                    a.frame.contains(r.x, r.y)
                        && a.frame.contains(r.x + r.w - 0.1, r.y + r.h - 0.1)
                );
            }
            for b in &placed[i + 1..] {
                assert!(!a.frame.overlaps(&b.frame));
            }
        }
        let b = bounds(&placed);
        assert!(b.h > placed[0].frame.h, "more than one shelf: {b:?}");
        assert!((b.w / b.h) < 4.0 && (b.w / b.h) > 0.4, "about a screen's shape: {b:?}");
    }

    #[test]
    fn connections_are_counted_between_frames_and_inside_them() {
        let m = components(&rows());
        let (between, within) = bundles(&m);
        // frames: Applications, Shared, Used by one, Probes, In no application
        assert!(between.contains(&(0, 1, 2)), "blog and shop both use store: {between:?}");
        assert!(between.contains(&(0, 2, 2)), "blog uses auth, shop uses cart: {between:?}");
        assert!(between.contains(&(2, 1, 1)), "cart uses store: {between:?}");
        assert_eq!(within.iter().sum::<usize>(), 0);
        assert_eq!(between.iter().map(|b| b.2).sum::<usize>(), m.edges.len());
    }

    #[test]
    fn what_uses_sits_left_of_what_it_uses() {
        // 0 uses 1 and 2, 1 uses 2 heavily, 2 uses 1 a little, 3 is on its own
        let between = [(0, 1, 9), (0, 2, 4), (1, 2, 5), (2, 1, 1)];
        assert_eq!(columns(4, &between), [0, 1, 2, 3]);
        assert_eq!(
            columns(2, &[(0, 1, 1), (1, 0, 1)]),
            [0, 1],
            "an even pair still has a way round"
        );
        let m = components(&rows());
        let placed = layout(&m);
        let x = |f: usize| placed[f].frame.x;
        assert!(x(0) < x(1), "the applications left of what they are made of");
    }

    #[test]
    fn a_line_leaves_a_rectangle_at_its_border() {
        let r = Rect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 };
        assert_eq!(exit(&r, 500.0, 25.0), (100.0, 25.0));
        assert_eq!(exit(&r, 50.0, -500.0), (50.0, 0.0));
        let (x, y) = exit(&r, 150.0, 75.0);
        assert!((y - 50.0).abs() < 1e-4 && (x - 100.0).abs() < 1e-4, "{x},{y}");
    }

    #[test]
    fn a_text_names_a_component_by_its_path_or_its_whole_name() {
        let names: BTreeSet<String> =
            ["rate-limiter", "eshop-gateway", "store"].map(String::from).into();
        let got = |t: &str| mentions(t, &names).into_iter().collect::<Vec<_>>();
        assert_eq!(got("write_file components/rate-limiter/src/lib.rs"), ["rate-limiter"]);
        assert_eq!(got("Wire rate-limiter into eshop-gateway."), ["eshop-gateway", "rate-limiter"]);
        assert!(
            got("a limiter in the store").is_empty(),
            "single words are too common to be names"
        );
        assert_eq!(got("components/store/wit"), ["store"], "a path is a name, hyphen or not");
    }

    #[test]
    fn a_run_is_its_attempts_with_their_fitness_and_what_it_added() {
        let rows = KnowledgeRows {
            runs: vec![RunRow {
                id: "77/g1".into(),
                goal: "add pagination to the search box".into(),
                outcome: "merged".into(),
                winner: "mvp".into(),
                url: "https://example.test/pull/1".into(),
                branches: 2,
                started: "2026-10-19T19:01:55Z".into(),
            }],
            attempts: vec![
                AttemptRow {
                    id: "77/g1/risk-first".into(),
                    run: "77/g1".into(),
                    branch: "risk-first".into(),
                    round: 1,
                    outcome: "failed".into(),
                    score: 40,
                    failing: vec![("test_pages".into(), "expected 3 pages".into())],
                    ..Default::default()
                },
                AttemptRow {
                    id: "77/g1/mvp".into(),
                    run: "77/g1".into(),
                    branch: "mvp".into(),
                    round: 1,
                    outcome: "passed".into(),
                    score: 100,
                    paths: vec!["components/paginate/src/lib.rs".into()],
                    ..Default::default()
                },
            ],
            capabilities: vec![("paginate".into(), "77/g1".into())],
        };
        let m = knowledge(&rows);
        assert_eq!(m.frames.len(), 1);
        let f = &m.frames[0];
        assert_eq!(f.sub, "merged · 2 attempts · best fitness 100");
        let labels: Vec<&str> = f.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["round 1 · mvp", "round 1 · risk-first", "paginate"]);
        assert_eq!(f.items[0].sub, "won · passed · fitness 100");
        assert!(matches!(
            &f.items[0].proof[..],
            [Proof::Check { passed: true, .. }, Proof::Link { .. }]
        ));
        assert!(
            matches!(&f.items[1].proof[..], [Proof::Check { passed: false, id, .. }] if id == "test_pages")
        );
        assert_eq!((f.items[0].kind, f.items[1].kind), (Kind::Attempt, Kind::Missed));
        assert_eq!(
            f.items[2].key,
            component_key("paginate"),
            "the same key as on the components tab"
        );
        assert_eq!(
            m.edges,
            [("attempt:77/g1/mvp".to_string(), component_key("paginate"), "wrote".to_string())]
        );
    }
}
