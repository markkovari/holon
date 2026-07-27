//! `wit-reflect` — reference implementation of `wit:reflect`.
//!
//! Three layers, and only the middle one is interesting:
//!
//! * `inspector` reads a component's import/export sections with `wasmparser`.
//!   Interface names in a component binary are already the strings everything
//!   else keys on (`records:store/store@0.1.0`), so this is a parse, not an
//!   inference — no WIT resolver, no source tree, no regex.
//! * `composer::plan` is plain graph work: order the plugs, find the gaps, refuse
//!   the cycles, count the instances.
//! * `composer::satisfies` and `composer::compose` delegate to `wac-graph`, which
//!   is what the `wac` CLI itself is built on. `satisfies` runs its
//!   `SubtypeChecker`, so a UI's connection validation is the real type check;
//!   `compose` runs its `plug`, so the artifact is the artifact `wac plug` writes.
//!
//! What a built component does NOT tell you: anything about itself. The embedded
//! type says `package root:component; world root { ... }` — the world name and
//! package from the source WIT are gone. And since these build for
//! `wasm32-wasip2`, there is no `component-name` custom section either (that came
//! from cargo-component's adapter path; `wasm-component-ld` writes none). So
//! identity is ONLY what it exports — a capability is recognisable because it
//! exports `records:store/store`, an app exporting just `wasi:http` is anonymous —
//! which is why every caller supplies its own id.

#[allow(warnings)]
mod bindings;
mod emit;

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use bindings::exports::wit::reflect::composer::{
    ComposeError, Edge, Gap, Guest as ComposerGuest, CompositionPlan, Node, Part, PlugStep, Problem,
    WorkloadMeta,
};
use bindings::exports::wit::reflect::inspector::{
    Guest as InspectorGuest, IfaceRef, ReflectError, Surface,
};

struct Component;

/// Namespaces only a runtime can satisfy. Everything else is a composition edge.
///
/// This is the distinction `tools/gen-catalog.py` gets wrong: it treats a subset
/// of `wasi:*` as "std" and files `wasi:keyvalue`, `wasi:config` and
/// `wasi:blobstore` alongside real component contracts, so the catalog cannot say
/// which imports `wac plug` is able to erase.
const HOST_NAMESPACES: &[&str] = &["wasi", "wasmcloud"];

/// wasmtime refuses to instantiate a component nesting more than this many
/// component instances. It is why `vet-domain` cannot be fused in one piece
/// (104 modules) and had to keep its stateful capabilities as links.
const NESTED_INSTANCE_LIMIT: u32 = 30;

// ---- inspector --------------------------------------------------------------

impl InspectorGuest for Component {
    fn inspect(bytes: Vec<u8>) -> Result<Surface, ReflectError> {
        if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
            return Err(ReflectError::NotAComponent("not wasm (bad magic)".into()));
        }
        // Byte 6 distinguishes a component (0x01) from a core module (0x00) in
        // the version/layer field. Saying so beats "parse error".
        if bytes[6] != 0x01 {
            return Err(ReflectError::NotAComponent(
                "this is a core wasm module, not a component".into(),
            ));
        }

        let mut imports_raw: Vec<String> = Vec::new();
        let mut exports_raw: Vec<String> = Vec::new();
        let mut name = String::new();
        let mut nested: u32 = 0;
        // Only the OUTER component's sections describe this component; a composed
        // artifact contains its plugs' sections too, and counting those would
        // report every capability's imports as if they were still unsatisfied.
        let mut depth: i32 = 0;

        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            let payload = payload.map_err(|e| ReflectError::BadWasm(e.to_string()))?;
            use wasmparser::Payload::*;
            match payload {
                ModuleSection { .. } | ComponentSection { .. } => {
                    depth += 1;
                    nested += 1;
                }
                End(_) => depth -= 1,
                ComponentImportSection(reader) if depth == 0 => {
                    for import in reader {
                        let import = import.map_err(|e| ReflectError::BadWasm(e.to_string()))?;
                        imports_raw.push(import.name.0.to_string());
                    }
                }
                ComponentExportSection(reader) if depth == 0 => {
                    for export in reader {
                        let export = export.map_err(|e| ReflectError::BadWasm(e.to_string()))?;
                        exports_raw.push(export.name.0.to_string());
                    }
                }
                CustomSection(c) if c.name() == "component-name" && depth == 0 => {
                    // Best-effort: a missing or odd name section is not an error.
                    name = component_name(c.data()).unwrap_or_default();
                }
                _ => {}
            }
        }

        let (host_imports, imports): (Vec<IfaceRef>, Vec<IfaceRef>) = imports_raw
            .iter()
            .map(|r| parse_ref(r))
            .partition(|r| HOST_NAMESPACES.contains(&r.namespace.as_str()));

        Ok(Surface {
            name,
            exports: exports_raw.iter().map(|r| parse_ref(r)).collect(),
            imports,
            host_imports,
            size_bytes: bytes.len() as u64,
            sha256: hex12(&bytes),
            nested_instances: nested,
        })
    }
}

/// Split `ns:pkg/iface@ver` into parts, keeping the raw string authoritative.
/// Names that don't match that shape (a bare function import, a `locked-dep=`
/// form) keep `raw` and land in `name` — never silently dropped.
fn parse_ref(raw: &str) -> IfaceRef {
    let (before_at, version) = match raw.rsplit_once('@') {
        // Guard against a `@` inside a path-ish name: a version starts with a digit.
        Some((b, v)) if v.starts_with(|c: char| c.is_ascii_digit()) => (b, v.to_string()),
        _ => (raw, String::new()),
    };
    let (pkg_part, iface) = match before_at.split_once('/') {
        Some((p, i)) => (p, i.to_string()),
        None => (before_at, String::new()),
    };
    let (namespace, package) = match pkg_part.split_once(':') {
        Some((n, p)) => (n.to_string(), p.to_string()),
        None => (String::new(), pkg_part.to_string()),
    };
    IfaceRef {
        raw: raw.to_string(),
        namespace,
        pkg: package,
        name: if iface.is_empty() { pkg_part.to_string() } else { iface },
        version,
    }
}

/// The component name from a `component-name` custom section: subsection id 0,
/// then a name-map-free plain string (LEB128 length + bytes).
fn component_name(data: &[u8]) -> Option<String> {
    let mut i = 0usize;
    while i < data.len() {
        let id = data[i];
        i += 1;
        let (size, used) = leb128(&data[i..])?;
        i += used;
        let end = i.checked_add(size)?;
        let body = data.get(i..end)?;
        if id == 0 {
            let (len, used) = leb128(body)?;
            return body.get(used..used + len).and_then(|b| String::from_utf8(b.to_vec()).ok());
        }
        i = end;
    }
    None
}

fn leb128(b: &[u8]) -> Option<(usize, usize)> {
    let (mut result, mut shift, mut used) = (0usize, 0u32, 0usize);
    loop {
        let byte = *b.get(used)?;
        used += 1;
        result |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return Some((result, used));
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
}

fn hex12(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().take(6).map(|b| format!("{b:02x}")).collect()
}

// ---- composer ---------------------------------------------------------------

impl ComposerGuest for Component {
    fn satisfies(socket: Vec<u8>, plug: Vec<u8>) -> Result<Vec<String>, ReflectError> {
        use wac_graph::types::{Package, SubtypeChecker};
        use wac_graph::CompositionGraph;

        let mut graph = CompositionGraph::new();
        let bad = |e: String| ReflectError::BadWasm(e);
        let socket_pkg = Package::from_bytes("socket", None, socket, graph.types_mut())
            .map_err(|e| bad(e.to_string()))?;
        let plug_pkg = Package::from_bytes("plug", None, plug, graph.types_mut())
            .map_err(|e| bad(e.to_string()))?;
        let socket_id = graph.register_package(socket_pkg).map_err(|e| bad(e.to_string()))?;
        let plug_id = graph.register_package(plug_pkg).map_err(|e| bad(e.to_string()))?;

        // Exactly the test wac-graph's `plug` applies: the plug's export type must
        // be a SUBTYPE of the socket's import type. Matching names is not enough —
        // two `foo:bar/baz@0.1.0` interfaces with different function signatures do
        // not fit, and this is where you find out.
        let mut cache = Default::default();
        let mut checker = SubtypeChecker::new(&mut cache);
        let mut out = Vec::new();
        for (name, plug_ty) in &graph.types()[graph[plug_id].ty()].exports {
            if let Some(socket_ty) = graph.types()[graph[socket_id].ty()].imports.get(name) {
                if checker.is_subtype(*plug_ty, graph.types(), *socket_ty, graph.types()).is_ok() {
                    out.push(name.clone());
                }
            }
        }
        Ok(out)
    }

    fn plan(nodes: Vec<Node>, edges: Vec<Edge>) -> CompositionPlan {
        let graph = Graph::build(&nodes, &edges);
        graph.into_plan()
    }

    fn compose(parts: Vec<Part>, edges: Vec<Edge>, root: String) -> Result<Vec<u8>, ComposeError> {
        let bytes: BTreeMap<&str, &[u8]> =
            parts.iter().map(|p| (p.id.as_str(), p.bytes.as_slice())).collect();
        if !bytes.contains_key(root.as_str()) {
            return Err(ComposeError::MissingPart(root));
        }
        // socket -> its direct plugs, in the order the caller drew them.
        let mut deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for e in &edges {
            if !bytes.contains_key(e.plug.as_str()) {
                return Err(ComposeError::MissingPart(e.plug.clone()));
            }
            if !bytes.contains_key(e.socket.as_str()) {
                return Err(ComposeError::MissingPart(e.socket.clone()));
            }
            let plugs = deps.entry(e.socket.as_str()).or_default();
            if !plugs.contains(&e.plug.as_str()) {
                plugs.push(e.plug.as_str());
            }
        }
        let mut memo: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut stack: Vec<&str> = Vec::new();
        compose_node(&root, &bytes, &deps, &mut memo, &mut stack)
    }

    fn emit_plug_script(p: CompositionPlan, out_dir: String) -> String {
        emit::plug_script(&p, &out_dir)
    }

    fn emit_wac(nodes: Vec<Node>, edges: Vec<Edge>, p: CompositionPlan, package_name: String) -> String {
        emit::wac_file(&nodes, &edges, &p, &package_name)
    }

    fn emit_workload(nodes: Vec<Node>, p: CompositionPlan, meta: WorkloadMeta) -> String {
        emit::workload(&nodes, &p, &meta)
    }
}

/// Compose one node: its plugs first (so an intermediate artifact is a real
/// composed component, exactly as the two-step Justfile recipes do it), then
/// `wac`'s `plug` over the results. Memoised, so a capability shared by two
/// consumers is composed once — though each consumer still gets its own
/// INSTANCE, which is `wac plug` semantics and the reason `.wac` exists.
fn compose_node<'a>(
    id: &'a str,
    bytes: &BTreeMap<&'a str, &'a [u8]>,
    deps: &BTreeMap<&'a str, Vec<&'a str>>,
    memo: &mut BTreeMap<String, Vec<u8>>,
    stack: &mut Vec<&'a str>,
) -> Result<Vec<u8>, ComposeError> {
    if let Some(done) = memo.get(id) {
        return Ok(done.clone());
    }
    if stack.contains(&id) {
        return Err(ComposeError::Unbuildable(format!(
            "cycle through `{id}` — a static composition cannot contain one (a wasmCloud workload can)"
        )));
    }
    let own = *bytes.get(id).ok_or_else(|| ComposeError::MissingPart(id.to_string()))?;
    let plugs = match deps.get(id) {
        Some(p) if !p.is_empty() => p.clone(),
        // A leaf: nothing to plug, its own bytes are the answer.
        _ => return Ok(own.to_vec()),
    };

    stack.push(id);
    let mut plug_bytes: Vec<(String, Vec<u8>)> = Vec::new();
    for plug in &plugs {
        // NOTE: the recursion is what makes depth > 2 work, which the hand-written
        // recipes never attempt (they stop at cache.composed.wasm).
        let composed = compose_node(plug, bytes, deps, memo, stack)?;
        plug_bytes.push(((*plug).to_string(), composed));
    }
    stack.pop();

    let out = plug_together(id, own, &plug_bytes)?;
    memo.insert(id.to_string(), out.clone());
    Ok(out)
}

fn plug_together(
    socket_id: &str,
    socket: &[u8],
    plugs: &[(String, Vec<u8>)],
) -> Result<Vec<u8>, ComposeError> {
    use wac_graph::types::Package;
    use wac_graph::{CompositionGraph, EncodeOptions};

    let mut graph = CompositionGraph::new();
    let socket_pkg = Package::from_bytes(socket_id, None, socket.to_vec(), graph.types_mut())
        .map_err(|e| ComposeError::PlugFailed(format!("{socket_id}: {e}")))?;
    let socket_pkg_id = graph
        .register_package(socket_pkg)
        .map_err(|e| ComposeError::PlugFailed(format!("{socket_id}: {e}")))?;

    let mut plug_ids = Vec::new();
    for (name, bytes) in plugs {
        let pkg = Package::from_bytes(name, None, bytes.clone(), graph.types_mut())
            .map_err(|e| ComposeError::PlugFailed(format!("{name}: {e}")))?;
        plug_ids.push(
            graph
                .register_package(pkg)
                .map_err(|e| ComposeError::PlugFailed(format!("{name}: {e}")))?,
        );
    }

    wac_graph::plug(&mut graph, plug_ids, socket_pkg_id).map_err(|e| {
        ComposeError::PlugFailed(format!(
            "{socket_id}: {e} (nothing the plugs export is imported by this socket)"
        ))
    })?;
    graph
        .encode(EncodeOptions::default())
        .map_err(|e| ComposeError::EncodeFailed(e.to_string()))
}

// ---- the planning graph -----------------------------------------------------

/// Everything `plan` needs, resolved once.
pub(crate) struct Graph<'a> {
    nodes: &'a [Node],
    by_id: BTreeMap<&'a str, &'a Node>,
    /// socket -> plugs
    deps: BTreeMap<&'a str, Vec<&'a str>>,
    /// (socket, plug) -> the interfaces the caller drew
    drawn: BTreeMap<(&'a str, &'a str), BTreeSet<&'a str>>,
    /// (socket, iface) the caller drew, for gap detection
    covered: BTreeSet<(&'a str, &'a str)>,
    problems: Vec<Problem>,
}

impl<'a> Graph<'a> {
    fn build(nodes: &'a [Node], edges: &'a [Edge]) -> Self {
        let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut g = Graph {
            nodes,
            by_id,
            deps: BTreeMap::new(),
            drawn: BTreeMap::new(),
            covered: BTreeSet::new(),
            problems: Vec::new(),
        };
        for e in edges {
            let problem = |kind: &str, detail: String| Problem { kind: kind.into(), detail };
            let (Some(plug), Some(socket)) =
                (g.by_id.get(e.plug.as_str()), g.by_id.get(e.socket.as_str()))
            else {
                g.problems.push(problem(
                    "unknown-node",
                    format!("edge {} -> {} names a node that isn't on the canvas", e.plug, e.socket),
                ));
                continue;
            };
            if e.plug == e.socket {
                g.problems
                    .push(problem("self-plug", format!("`{}` cannot plug into itself", e.plug)));
                continue;
            }
            if !plug.surface.exports.iter().any(|x| x.raw == e.iface) {
                g.problems.push(problem(
                    "not-exported",
                    format!("`{}` does not export {}", e.plug, e.iface),
                ));
                continue;
            }
            if socket.surface.host_imports.iter().any(|x| x.raw == e.iface) {
                g.problems.push(problem(
                    "host-import-edge",
                    format!(
                        "{} is a host capability — a runtime provides it, a component cannot",
                        e.iface
                    ),
                ));
                continue;
            }
            if !socket.surface.imports.iter().any(|x| x.raw == e.iface) {
                g.problems.push(problem(
                    "not-imported",
                    format!("`{}` does not import {}", e.socket, e.iface),
                ));
                continue;
            }
            let plugs = g.deps.entry(socket.id.as_str()).or_default();
            if !plugs.contains(&plug.id.as_str()) {
                plugs.push(plug.id.as_str());
            }
            g.drawn
                .entry((socket.id.as_str(), plug.id.as_str()))
                .or_default()
                .insert(e.iface.as_str());
            g.covered.insert((socket.id.as_str(), e.iface.as_str()));
        }
        g
    }

    /// Depth-first cycle detection over socket -> plug dependencies.
    fn find_cycle(&self) -> Option<Vec<&'a str>> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Open,
            Done,
        }
        let mut marks: BTreeMap<&str, Mark> = BTreeMap::new();
        let mut path: Vec<&str> = Vec::new();

        fn walk<'b>(
            id: &'b str,
            deps: &BTreeMap<&'b str, Vec<&'b str>>,
            marks: &mut BTreeMap<&'b str, Mark>,
            path: &mut Vec<&'b str>,
        ) -> Option<Vec<&'b str>> {
            match marks.get(id) {
                Some(Mark::Done) => return None,
                Some(Mark::Open) => {
                    let mut cycle = path.clone();
                    cycle.push(id);
                    return Some(cycle);
                }
                None => {}
            }
            marks.insert(id, Mark::Open);
            path.push(id);
            for next in deps.get(id).into_iter().flatten() {
                if let Some(c) = walk(next, deps, marks, path) {
                    return Some(c);
                }
            }
            path.pop();
            marks.insert(id, Mark::Done);
            None
        }

        for node in self.nodes {
            if let Some(c) = walk(node.id.as_str(), &self.deps, &mut marks, &mut path) {
                return Some(c);
            }
        }
        None
    }

    /// Topological depth: a leaf is 0, everything else is 1 + its deepest plug.
    /// Doubles as the UI's layout — column = depth, no layout library needed.
    fn depths(&self) -> BTreeMap<&'a str, u32> {
        let mut depth: BTreeMap<&str, u32> = BTreeMap::new();
        // Iterating |nodes| times settles any DAG; a cycle is caught before this.
        for _ in 0..=self.nodes.len() {
            let mut changed = false;
            for node in self.nodes {
                let id = node.id.as_str();
                let want = self
                    .deps
                    .get(id)
                    .into_iter()
                    .flatten()
                    .map(|p| depth.get(p).copied().unwrap_or(0) + 1)
                    .max()
                    .unwrap_or(0);
                if depth.get(id).copied().unwrap_or(0) != want {
                    depth.insert(id, want);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for node in self.nodes {
            depth.entry(node.id.as_str()).or_insert(0);
        }
        depth
    }

    fn into_plan(mut self) -> CompositionPlan {
        let cycle = self.find_cycle();
        let cyclic = cycle.is_some();
        if let Some(c) = &cycle {
            self.problems.push(Problem {
                kind: "cycle".into(),
                detail: format!(
                    "{} — a static composition cannot contain a cycle; deploy it as a wasmCloud workload, where the runtime links at invoke time",
                    c.join(" -> ")
                ),
            });
        }

        let depth = self.depths();
        let plugged: BTreeSet<&str> =
            self.deps.values().flatten().copied().collect::<BTreeSet<_>>();
        let roots: Vec<String> = self
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| !plugged.contains(id))
            .map(String::from)
            .collect();

        // Build order: shallowest socket first. Stable within a depth by id.
        let mut sockets: Vec<&str> = self.deps.keys().copied().collect();
        sockets.sort_by_key(|id| (depth.get(id).copied().unwrap_or(0), *id));
        let steps: Vec<PlugStep> = if cyclic {
            Vec::new()
        } else {
            sockets
                .iter()
                .enumerate()
                .map(|(i, socket)| {
                    let plugs = self.deps.get(socket).cloned().unwrap_or_default();
                    let socket_node = self.by_id[*socket];
                    // Every interface wac WILL satisfy, minus the ones drawn: the
                    // caller cannot ask wac to satisfy only some of them.
                    let mut also = Vec::new();
                    for plug in &plugs {
                        let drawn = self.drawn.get(&(*socket, *plug)).cloned().unwrap_or_default();
                        for export in &self.by_id[*plug].surface.exports {
                            let imported = socket_node
                                .surface
                                .imports
                                .iter()
                                .any(|i| i.raw == export.raw);
                            if imported && !drawn.contains(export.raw.as_str()) {
                                also.push(export.raw.clone());
                            }
                        }
                    }
                    PlugStep {
                        order: i as u32,
                        socket: (*socket).to_string(),
                        plugs: plugs.iter().map(|p| (*p).to_string()).collect(),
                        output: format!("{socket}.composed.wasm"),
                        also_satisfies: also,
                    }
                })
                .collect()
        };

        // Gaps: a composable import with no edge into it survives into the
        // finished artifact, so the root's leftover imports are the union of these.
        let mut unsatisfied = Vec::new();
        for node in self.nodes {
            for import in &node.surface.imports {
                if !self.covered.contains(&(node.id.as_str(), import.raw.as_str())) {
                    unsatisfied.push(Gap { node: node.id.clone(), iface: import.clone() });
                }
            }
        }

        // Host needs: deduped union across the graph, since one workload's
        // hostInterfaces cover every component in it.
        let mut host_seen: BTreeSet<&str> = BTreeSet::new();
        let mut host_needs = Vec::new();
        for node in self.nodes {
            for h in &node.surface.host_imports {
                if host_seen.insert(h.raw.as_str()) {
                    host_needs.push(h.clone());
                }
            }
        }

        // Instance budget: each node contributes itself plus whatever it already
        // nests (a composed plug carries its own). An estimate, but the right
        // order of magnitude, and the limit it warns about is real.
        let instance_count: u32 =
            self.nodes.iter().map(|n| 1 + n.surface.nested_instances).sum();
        let over = instance_count > NESTED_INSTANCE_LIMIT;
        if over {
            self.problems.push(Problem {
                kind: "nested-instance-limit".into(),
                detail: format!(
                    "~{instance_count} nested instances exceeds wasmtime's limit of {NESTED_INSTANCE_LIMIT}: this composes but will fail to instantiate. Keep the stateful capabilities as runtime links instead of fusing them (see vet-domain-lattice)"
                ),
            });
        }
        if roots.len() > 1 && !self.deps.is_empty() {
            self.problems.push(Problem {
                kind: "multiple-roots".into(),
                detail: format!(
                    "{} components have nothing plugged into them ({}) — a static composition has exactly one root; the rest are separate artifacts",
                    roots.len(),
                    roots.join(", ")
                ),
            });
        }

        CompositionPlan {
            steps,
            unsatisfied,
            host_needs,
            cyclic,
            instance_count,
            over_instance_limit: over,
            depth: depth.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            roots,
            problems: self.problems,
        }
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(raw: &str) -> IfaceRef {
        parse_ref(raw)
    }

    fn surface(name: &str, exports: &[&str], imports: &[&str], host: &[&str]) -> Surface {
        Surface {
            name: name.into(),
            exports: exports.iter().map(|r| iface(r)).collect(),
            imports: imports.iter().map(|r| iface(r)).collect(),
            host_imports: host.iter().map(|r| iface(r)).collect(),
            size_bytes: 1000,
            sha256: "aaaaaaaaaaaa".into(),
            nested_instances: 1,
        }
    }

    fn node(id: &str, s: Surface) -> Node {
        Node { id: id.into(), surface: s }
    }

    fn edge(plug: &str, socket: &str, iface: &str) -> Edge {
        Edge { plug: plug.into(), socket: socket.into(), iface: iface.into() }
    }

    #[test]
    fn parses_interface_names() {
        let r = iface("records:store/store@0.1.0");
        assert_eq!((r.namespace.as_str(), r.pkg.as_str(), r.name.as_str(), r.version.as_str()),
                   ("records", "store", "store", "0.1.0"));
        let r = iface("wasi:keyvalue/store@0.2.0-draft");
        assert_eq!(r.version, "0.2.0-draft", "prerelease versions survive intact");
        // gen-catalog.py drops these; we keep them, because wac and wasmCloud
        // both key on the exact string.
        let r = iface("auth:identity/types");
        assert_eq!((r.name.as_str(), r.version.as_str()), ("types", ""));
        let r = iface("some-bare-name");
        assert_eq!(r.name, "some-bare-name", "an unparseable name is kept, not dropped");
    }

    #[test]
    fn plans_a_two_level_build_in_order() {
        // app <- cache <- cache-backing, the shape four recipes hand-write.
        let nodes = vec![
            node("app", surface("app", &["wasi:http/incoming-handler@0.2.0"], &["cache:store/cache@0.1.0"], &["wasi:clocks/wall-clock@0.2.0"])),
            node("cache", surface("cache", &["cache:store/cache@0.1.0"], &["cache:store/source@0.1.0"], &["wasi:keyvalue/store@0.2.0-draft"])),
            node("backing", surface("cache-backing", &["cache:store/source@0.1.0"], &[], &[])),
        ];
        let edges = vec![
            edge("cache", "app", "cache:store/cache@0.1.0"),
            edge("backing", "cache", "cache:store/source@0.1.0"),
        ];
        let p = <Component as ComposerGuest>::plan(nodes, edges);

        assert!(!p.cyclic);
        assert_eq!(p.roots, vec!["app"], "nothing plugs into the app");
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[0].socket, "cache", "the deeper socket is built first");
        assert_eq!(p.steps[1].socket, "app");
        assert!(p.unsatisfied.is_empty(), "every composable import has an edge");
        // Host capabilities are NOT gaps — no component can satisfy them.
        assert_eq!(p.host_needs.len(), 2);
        let depth: BTreeMap<_, _> = p.depth.into_iter().collect();
        assert_eq!((depth["backing"], depth["cache"], depth["app"]), (0, 1, 2), "layout columns");
    }

    #[test]
    fn reports_unsatisfied_imports_as_gaps() {
        let nodes = vec![
            node("app", surface("app", &[], &["a:b/c@0.1.0", "d:e/f@0.1.0"], &[])),
            node("cap", surface("cap", &["a:b/c@0.1.0"], &[], &[])),
        ];
        let p = <Component as ComposerGuest>::plan(nodes, vec![edge("cap", "app", "a:b/c@0.1.0")]);
        assert_eq!(p.unsatisfied.len(), 1);
        assert_eq!(p.unsatisfied[0].iface.raw, "d:e/f@0.1.0");
        assert_eq!(p.unsatisfied[0].node, "app");
    }

    #[test]
    fn warns_that_wac_satisfies_more_than_you_drew() {
        // One plug exporting two interfaces the socket imports: `wac plug` wires
        // BOTH whether you drew one edge or two. A UI that hides this is lying.
        let nodes = vec![
            node("app", surface("app", &[], &["auth:identity/authorizer@0.1.0", "auth:identity/accounts@0.1.0"], &[])),
            node("guard", surface("auth-guard", &["auth:identity/authorizer@0.1.0", "auth:identity/accounts@0.1.0"], &[], &[])),
        ];
        let p = <Component as ComposerGuest>::plan(
            nodes,
            vec![edge("guard", "app", "auth:identity/authorizer@0.1.0")],
        );
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].also_satisfies, vec!["auth:identity/accounts@0.1.0"]);
        // ...and so it is not reported as a gap either.
        assert!(p.unsatisfied.iter().any(|g| g.iface.name == "accounts"),
                "still listed as undrawn until the user wires it");
    }

    #[test]
    fn refuses_a_cycle_but_keeps_the_graph() {
        let nodes = vec![
            node("a", surface("a", &["x:y/z@0.1.0"], &["p:q/r@0.1.0"], &[])),
            node("b", surface("b", &["p:q/r@0.1.0"], &["x:y/z@0.1.0"], &[])),
        ];
        let p = <Component as ComposerGuest>::plan(
            nodes,
            vec![edge("a", "b", "x:y/z@0.1.0"), edge("b", "a", "p:q/r@0.1.0")],
        );
        assert!(p.cyclic);
        assert!(p.steps.is_empty(), "no static build order exists");
        assert!(p.problems.iter().any(|pr| pr.kind == "cycle"));
        // The point: it's still deployable as a workload, and the message says so.
        assert!(p.problems.iter().any(|pr| pr.detail.contains("workload")));
    }

    #[test]
    fn rejects_edges_that_cannot_exist() {
        let nodes = vec![
            node("app", surface("app", &[], &["a:b/c@0.1.0"], &["wasi:keyvalue/store@0.2.0-draft"])),
            node("cap", surface("cap", &["a:b/c@0.1.0"], &[], &[])),
        ];
        let cases: Vec<(Edge, &str)> = vec![
            (edge("cap", "app", "nope:nope/nope@1.0.0"), "not-exported"),
            (edge("cap", "ghost", "a:b/c@0.1.0"), "unknown-node"),
            (edge("app", "app", "a:b/c@0.1.0"), "self-plug"),
        ];
        for (e, kind) in cases {
            let p = <Component as ComposerGuest>::plan(nodes.clone(), vec![e]);
            assert!(p.problems.iter().any(|pr| pr.kind == kind), "expected {kind}: {:?}", p.problems);
        }
        // A host capability is not a composition edge, however you draw it.
        let host = node("kv", surface("kv", &["wasi:keyvalue/store@0.2.0-draft"], &[], &[]));
        let mut with_host = nodes.clone();
        with_host.push(host);
        let p = <Component as ComposerGuest>::plan(
            with_host,
            vec![edge("kv", "app", "wasi:keyvalue/store@0.2.0-draft")],
        );
        assert!(p.problems.iter().any(|pr| pr.kind == "host-import-edge"));
    }

    #[test]
    fn warns_before_the_instance_limit_bites() {
        // vet-domain's real shape: one app, 19 capabilities, ~104 modules. It
        // composes and then refuses to instantiate — the warning is the product.
        let mut nodes = vec![node(
            "app",
            Surface { nested_instances: 4, ..surface("app", &[], &[], &[]) },
        )];
        for i in 0..19 {
            nodes.push(node(
                &format!("cap{i}"),
                Surface { nested_instances: 3, ..surface("cap", &[], &[], &[]) },
            ));
        }
        let p = <Component as ComposerGuest>::plan(nodes, vec![]);
        assert!(p.over_instance_limit, "{} instances", p.instance_count);
        assert!(p.problems.iter().any(|pr| pr.kind == "nested-instance-limit"));
    }

    // ---- against real artifacts from this repo ------------------------------

    fn artifact(name: &str) -> Option<Vec<u8>> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/wasm32-wasip2/release")
            .join(format!("{name}.wasm"));
        std::fs::read(p).ok()
    }

    /// Panics with a usable message rather than skipping silently.
    fn require(name: &str) -> Vec<u8> {
        artifact(name).unwrap_or_else(|| {
            panic!("components/target/wasm32-wasip2/release/{name}.wasm missing — run `just build` first")
        })
    }

    #[test]
    fn inspects_a_real_component() {
        let bytes = require("mesh_domain");
        let s = <Component as InspectorGuest>::inspect(bytes).expect("mesh-domain inspects");
        // A wasm32-wasip2 artifact is ANONYMOUS: `wasm-component-ld` writes no
        // `component-name` section (nor a producers tag), where cargo-component's
        // adapter path did. A built component carries no identity of its own —
        // which is why every caller downstream supplies an id.
        assert!(s.name.is_empty(), "p2 artifacts carry no name section, got {:?}", s.name);
        assert_eq!(s.exports.len(), 1);
        assert_eq!(s.exports[0].raw, "wasi:http/incoming-handler@0.2.0");
        let composable: Vec<&str> = s.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(composable.contains(&"records:store/store@0.1.0"));
        assert!(composable.contains(&"resilience:breaker/breaker@0.1.0"));
        assert!(composable.contains(&"proxy:route/router@0.1.0"));
        assert_eq!(composable.len(), 3, "exactly the three plugs its recipe uses: {composable:?}");
        // ...and everything else needs a host, which no component can supply.
        assert!(s.host_imports.iter().all(|h| h.namespace == "wasi"));
        assert!(s.host_imports.iter().any(|h| h.raw == "wasi:keyvalue/store@0.2.0-draft")
            || s.host_imports.iter().any(|h| h.name == "wall-clock"));
    }

    #[test]
    fn a_composed_artifact_reports_only_what_is_left() {
        // The distinction depth-tracking buys: a composed component contains its
        // plugs' import sections, and counting those would report satisfied
        // imports as outstanding.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/mesh_domain.composed.wasm");
        let Ok(bytes) = std::fs::read(path) else {
            panic!("components/target/mesh_domain.composed.wasm missing — run `just compose-mesh`")
        };
        let s = <Component as InspectorGuest>::inspect(bytes).expect("composed inspects");
        assert!(s.imports.is_empty(), "nothing composable is left: {:?}", s.imports);
        assert!(s.nested_instances > 1, "it nests its plugs");
    }

    #[test]
    fn rejects_a_core_module_and_junk() {
        let core = b"\0asm\x01\0\0\0".to_vec();
        assert!(matches!(
            <Component as InspectorGuest>::inspect(core),
            Err(ReflectError::NotAComponent(_))
        ));
        assert!(matches!(
            <Component as InspectorGuest>::inspect(b"not wasm at all".to_vec()),
            Err(ReflectError::NotAComponent(_))
        ));
        // A truncated upload must not look like a valid component.
        let mut half = require("resilience");
        half.truncate(half.len() / 2);
        assert!(<Component as InspectorGuest>::inspect(half).is_err(), "truncation is caught");
    }

    #[test]
    fn satisfies_uses_the_real_subtype_check() {
        let mesh = require("mesh_domain");
        let records = require("record_store");
        let zip = require("zip");
        assert_eq!(
            <Component as ComposerGuest>::satisfies(mesh.clone(), records).unwrap(),
            vec!["records:store/store@0.1.0"]
        );
        // Nothing zip exports is imported by mesh — an honest empty answer, so a
        // UI can refuse the connection instead of composing something broken.
        assert!(<Component as ComposerGuest>::satisfies(mesh, zip).unwrap().is_empty());
    }

    #[test]
    fn composes_the_same_artifact_wac_plug_would() {
        let parts = vec![
            Part { id: "mesh".into(), bytes: require("mesh_domain") },
            Part { id: "records".into(), bytes: require("record_store") },
            Part { id: "resilience".into(), bytes: require("resilience") },
            Part { id: "proxy".into(), bytes: require("proxy_route") },
        ];
        let edges = vec![
            edge("records", "mesh", "records:store/store@0.1.0"),
            edge("resilience", "mesh", "resilience:breaker/breaker@0.1.0"),
            edge("proxy", "mesh", "proxy:route/router@0.1.0"),
        ];
        let out = <Component as ComposerGuest>::compose(parts, edges, "mesh".into())
            .expect("composes");

        // It is a component, and every composable import is gone.
        let s = <Component as InspectorGuest>::inspect(out.clone()).expect("output inspects");
        assert!(s.imports.is_empty(), "left over: {:?}", s.imports);
        assert!(s.host_imports.iter().any(|h| h.name == "incoming-handler" || h.namespace == "wasi"));
        assert_eq!(s.exports[0].raw, "wasi:http/incoming-handler@0.2.0");

        // Structurally what `wac plug` writes: the plugs are nested inside, so the
        // instance count went up and the artifact is bigger than the socket alone.
        //
        // NOT compared against components/target/mesh_domain.composed.wasm here.
        // `cargo test` doesn't rebuild that file, so a stale one would fail this
        // for no reason. The equality check against the CLI's own output belongs
        // in the e2e, where the Justfile has just rebuilt both.
        assert!(s.nested_instances >= 4, "socket + 3 plugs are nested in: {}", s.nested_instances);
        assert!(out.len() > require("mesh_domain").len(), "it contains its plugs");
    }

    #[test]
    fn compose_refuses_what_it_cannot_build() {
        let parts = vec![Part { id: "mesh".into(), bytes: require("mesh_domain") }];
        assert!(matches!(
            <Component as ComposerGuest>::compose(parts.clone(), vec![], "ghost".into()),
            Err(ComposeError::MissingPart(_))
        ));
        // An edge naming bytes we don't have.
        assert!(matches!(
            <Component as ComposerGuest>::compose(
                parts,
                vec![edge("nope", "mesh", "records:store/store@0.1.0")],
                "mesh".into()
            ),
            Err(ComposeError::MissingPart(_))
        ));
        // A plug whose exports the socket doesn't import: wac says no.
        let parts = vec![
            Part { id: "mesh".into(), bytes: require("mesh_domain") },
            Part { id: "zip".into(), bytes: require("zip") },
        ];
        assert!(matches!(
            <Component as ComposerGuest>::compose(
                parts,
                vec![edge("zip", "mesh", "zip:archive/archiver@0.1.0")],
                "mesh".into()
            ),
            Err(ComposeError::PlugFailed(_))
        ));
    }
}
