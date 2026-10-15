//! Composition as a library call: wrap `wac_graph`, do not run the `wac` CLI.
//!
//! A component states its imports and every capability states what it exports, which is a complete
//! wiring diagram, so no hand-written `wac plug` chains are needed. Calling `wac_graph` directly
//! gives which plug satisfies which import and what is unsatisfied as values, in-process.
//!
//! * **A flat plug chain is not a composition.** `wac plug root --plug a --plug b` satisfies the
//!   ROOT's imports and hoists each plug's own imports into the result, and `wasm-tools validate`
//!   is still happy. [`compose`] recurses so every plug goes in whole.
//! * **Resolution is per-interface, not per-package.** `cache-backing` exports `cache:store/sink`
//!   but not `cache:store/cache`, so a package-level match reports "satisfied" for a dangling import.
//!
//! It does not decide whether a plug's TYPES fit; `wac_graph::plug` refuses what does not.
//! `components/wit-reflect` is the sandboxed counterpart; this is the native side for the loop.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Namespaces only a HOST can satisfy: a runtime capability, never a composition edge.
const HOST_NAMESPACES: &[&str] = &["wasi", "wasmcloud", "comp"];

/// What a component imports and exports, read out of the BINARY, not `components/*/wit/`: some
/// components have no wit directory, a source directory can declare a package it does not export,
/// and the compiler DROPS an import nothing calls.
#[derive(Debug, Clone, Default)]
pub struct Surface {
    /// Interfaces it exports: what it can be plugged in to satisfy.
    pub exports: BTreeSet<String>,
    /// Imports another component could satisfy — the composable edges.
    pub imports: BTreeSet<String>,
    /// Imports only a host can satisfy.
    pub host_imports: BTreeSet<String>,
    /// Nested component instances already inside this artifact.
    pub nested_instances: u32,
}

/// Read a component's surface. `Err` for anything that is not a component —
/// adapters and core modules live in the same directory and are not plugs.
pub fn surface(bytes: &[u8]) -> Result<Surface, String> {
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
        return Err("not wasm (bad magic)".into());
    }
    // Byte 6 distinguishes a component (0x01) from a core module (0x00).
    if bytes[6] != 0x01 {
        return Err("a core wasm module, not a component".into());
    }

    let mut out = Surface::default();
    // Only the OUTER component's sections describe THIS component; a composed artifact carries its
    // plugs' sections too, and counting those would report their imports as unsatisfied.
    let mut depth: i32 = 0;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        use wasmparser::Payload::*;
        match payload.map_err(|e| e.to_string())? {
            ModuleSection { .. } | ComponentSection { .. } => {
                depth += 1;
                out.nested_instances += 1;
            }
            End(_) => depth -= 1,
            ComponentImportSection(reader) if depth == 0 => {
                for import in reader {
                    let name = import.map_err(|e| e.to_string())?.name.0.to_string();
                    if HOST_NAMESPACES.contains(&namespace(&name)) {
                        out.host_imports.insert(name);
                    } else {
                        out.imports.insert(name);
                    }
                }
            }
            ComponentExportSection(reader) if depth == 0 => {
                for export in reader {
                    out.exports.insert(export.map_err(|e| e.to_string())?.name.0.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn namespace(iface: &str) -> &str {
    iface.split(':').next().unwrap_or(iface)
}

/// Component name (`clinic-domain`) from an artifact stem (`clinic_domain`).
fn crate_name(stem: &str) -> String {
    stem.replace('_', "-")
}

/// Every built component, and which interface each one exports.
#[derive(Debug, Default)]
pub struct Catalog {
    surfaces: BTreeMap<String, Surface>,
    bytes: BTreeMap<String, Vec<u8>>,
    /// interface -> the component exporting it.
    exporters: BTreeMap<String, String>,
}

impl Catalog {
    /// Read every component in `dirs`. Earlier directories win, so a gate that rebuilt one crate
    /// puts its own output first.
    pub fn scan(dirs: &[PathBuf]) -> Self {
        let mut me = Self::default();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            for path in paths {
                if path.extension().is_none_or(|e| e != "wasm") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
                let name = crate_name(stem);
                if me.surfaces.contains_key(&name) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else { continue };
                let Ok(surface) = surface(&bytes) else { continue };
                for export in &surface.exports {
                    me.exporters.entry(export.clone()).or_insert_with(|| name.clone());
                }
                me.surfaces.insert(name.clone(), surface);
                me.bytes.insert(name, bytes);
            }
        }
        me
    }

    pub fn surface(&self, name: &str) -> Option<&Surface> {
        self.surfaces.get(name)
    }

    pub fn bytes(&self, name: &str) -> Option<&[u8]> {
        self.bytes.get(name).map(Vec::as_slice)
    }

    /// Which component exports this interface.
    pub fn exporter(&self, iface: &str) -> Option<&str> {
        self.exporters.get(iface).map(String::as_str)
    }

    /// Every component in the catalogue, in name order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.surfaces.keys().map(String::as_str)
    }

    /// Imports nothing in the catalogue can satisfy.
    ///
    /// Not the same as "broken": an interface with no exporter is usually a types-only interface
    /// from the component's OWN package. What is worth knowing is an import from an unrelated
    /// package that nothing provides: that composition will always be incomplete.
    pub fn unmet(&self, name: &str) -> Vec<String> {
        let Some(surface) = self.surface(name) else { return Vec::new() };
        surface
            .imports
            .iter()
            .filter(|iface| self.exporter(iface).is_none())
            .filter(|iface| {
                let package = format!("{}/", iface.split('/').next().unwrap_or(iface));
                // Structural, not missing: the component's own package (`audit-log` imports
                // `audit:log/types`, exports `audit:log/query`), or a CONSUMER of a package whose
                // other interfaces are provided. A types-only interface has no implementation.
                let provided_here = surface.exports.iter().any(|e| e.starts_with(&package));
                let provided_somewhere = self.exporters.keys().any(|e| e.starts_with(&package));
                !provided_here && !provided_somewhere
            })
            .cloned()
            .collect()
    }

    /// Everything that ends up inside a composed artifact, transitively. Read backwards it answers
    /// "which apps am I inside": a capability's blast radius is every app that transitively
    /// composes it.
    pub fn closure(&self, name: &str) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut queue = vec![name.to_string()];
        while let Some(current) = queue.pop() {
            let Ok(w) = wiring(&current, self) else { continue };
            for plug in w.plugs {
                if seen.insert(plug.clone()) {
                    queue.push(plug);
                }
            }
        }
        seen.into_iter().collect()
    }

    /// Who consumes what, as edges: `(consumer, interface, provider)`.
    ///
    /// This is the capability graph, derived from the built artifacts every time because a
    /// hand-maintained dependency list goes wrong. An interface with a provider and no consumers
    /// yields no edge; see [`Catalog::orphan_exports`].
    pub fn edges(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for (consumer, surface) in &self.surfaces {
            for iface in &surface.imports {
                if let Some(provider) = self.exporter(iface) {
                    if provider != consumer {
                        out.push((consumer.clone(), iface.clone(), provider.to_string()));
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// Interfaces something exports that nothing in the tree imports. Not a finding on its own, but
    /// "may I change this interface?" differs for 0 consumers and for 37.
    pub fn orphan_exports(&self) -> Vec<(String, String)> {
        let consumed: BTreeSet<&String> =
            self.surfaces.values().flat_map(|s| s.imports.iter()).collect();
        let mut out: Vec<(String, String)> = self
            .exporters
            .iter()
            .filter(|(iface, _)| !consumed.contains(iface))
            .map(|(iface, owner)| (owner.clone(), iface.clone()))
            .collect();
        out.sort();
        out
    }

    /// How many components import this interface.
    pub fn consumer_count(&self, iface: &str) -> usize {
        self.surfaces.values().filter(|s| s.imports.contains(iface)).count()
    }

    pub fn len(&self) -> usize {
        self.surfaces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }
}

/// What composing a component would take.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wiring {
    /// Direct plugs, in the order their interfaces appear.
    pub plugs: Vec<String>,
    /// `(component, interface)`: an import nothing built can satisfy. Not an error on its own (a
    /// types-only interface from a component's own package has nothing to implement).
    pub missing: Vec<(String, String)>,
}

/// Who plugs into `name`, from what it imports.
pub fn wiring(name: &str, catalog: &Catalog) -> Result<Wiring, String> {
    let surface = catalog.surface(name).ok_or_else(|| format!("no built component '{name}'"))?;
    let mut out = Wiring::default();
    for import in &surface.imports {
        match catalog.exporter(import) {
            // A component may re-export what it imports; plugging it into itself is a cycle.
            Some(plug) if plug == name => {}
            Some(plug) => {
                if !out.plugs.iter().any(|p| p == plug) {
                    out.plugs.push(plug.to_string());
                }
            }
            None => out.missing.push((name.to_string(), import.clone())),
        }
    }
    Ok(out)
}

/// Compose `name` with everything it imports, recursively. Each plug is composed BEFORE it is
/// plugged, because a plug that is not whole leaves its own imports hoisted into the result.
pub fn compose(name: &str, catalog: &Catalog) -> Result<Vec<u8>, String> {
    compose_inner(name, catalog, &mut Vec::new())
}

fn compose_inner(
    name: &str,
    catalog: &Catalog,
    stack: &mut Vec<String>,
) -> Result<Vec<u8>, String> {
    if stack.iter().any(|s| s == name) {
        stack.push(name.to_string());
        return Err(format!("capability cycle: {}", stack.join(" → ")));
    }
    let own = catalog.bytes(name).ok_or_else(|| format!("no built component '{name}'"))?.to_vec();
    let Wiring { plugs, .. } = wiring(name, catalog)?;
    if plugs.is_empty() {
        return Ok(own);
    }

    stack.push(name.to_string());
    let mut composed_plugs = Vec::with_capacity(plugs.len());
    for plug in &plugs {
        composed_plugs.push((plug.clone(), compose_inner(plug, catalog, stack)?));
    }
    stack.pop();

    plug_together(name, &own, &composed_plugs)
}

/// One `wac_graph::plug`, which is what `wac plug` runs.
fn plug_together(
    socket_name: &str,
    socket: &[u8],
    plugs: &[(String, Vec<u8>)],
) -> Result<Vec<u8>, String> {
    use wac_graph::types::Package;
    use wac_graph::{CompositionGraph, EncodeOptions};

    let mut graph = CompositionGraph::new();
    let socket_pkg = Package::from_bytes(socket_name, None, socket.to_vec(), graph.types_mut())
        .map_err(|e| format!("{socket_name}: {e}"))?;
    let socket_id =
        graph.register_package(socket_pkg).map_err(|e| format!("{socket_name}: {e}"))?;

    let mut plug_ids = Vec::with_capacity(plugs.len());
    for (name, bytes) in plugs {
        let pkg = Package::from_bytes(name, None, bytes.clone(), graph.types_mut())
            .map_err(|e| format!("{name}: {e}"))?;
        plug_ids.push(graph.register_package(pkg).map_err(|e| format!("{name}: {e}"))?);
    }

    wac_graph::plug(&mut graph, plug_ids, socket_id)
        .map_err(|e| format!("{socket_name}: {e} (plugs: {})", names(plugs)))?;
    graph.encode(EncodeOptions::default()).map_err(|e| format!("{socket_name}: {e}"))
}

fn names(plugs: &[(String, Vec<u8>)]) -> String {
    plugs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
}

/// Compose and write the result, keyed by the content that went into it, so a gate that runs twenty
/// times composes once and the artifact outlives the run.
pub fn compose_to(name: &str, catalog: &Catalog, out_dir: &Path) -> Result<PathBuf, String> {
    let mut hasher = Sha256::new();
    hasher.update(catalog.bytes(name).ok_or_else(|| format!("no built component '{name}'"))?);
    let mut queue = vec![name.to_string()];
    let mut seen = BTreeSet::new();
    while let Some(current) = queue.pop() {
        for plug in wiring(&current, catalog)?.plugs {
            if seen.insert(plug.clone()) {
                hasher.update(catalog.bytes(&plug).unwrap_or_default());
                queue.push(plug);
            }
        }
    }
    let stamp: String = hasher.finalize()[..8].iter().map(|b| format!("{b:02x}")).collect();

    let out = out_dir.join(format!("{name}.{stamp}.wasm"));
    if out.is_file() {
        return Ok(out);
    }
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let bytes = compose(name, catalog)?;

    // Write somewhere else, then RENAME: `fs::write` creates the file before filling it, and the
    // `is_file()` check above would hand that empty file to the next caller (three `publish.rs`
    // tests racing on a zero-byte artifact; see `tests/compose_race.rs`).
    //
    // The temp name carries the process id and a counter because racers agree on `stamp` (the
    // digest of the same inputs).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = out_dir.join(format!(".{name}.{stamp}.{}.{seq}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    // Atomic within a directory. Losing the race is fine: both wrote the same bytes, the filename IS
    // their digest.
    if let Err(e) = std::fs::rename(&tmp, &out) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(out)
}

/// What a piece of work TOUCHES, as tags for the knowledge pool (ADR-0090).
///
/// Given the files a part may write, this names the component and every interface it imports, the
/// keys by which a later run with different wording can find a lesson.
///
/// Derived, never authored: a tag decides what future runs are shown, so it comes from the artifact
/// and not from a model that would like to be found (ADR-0084).
pub fn tags_for(writable: &[String], catalog: &Catalog) -> Vec<String> {
    let mut tags = BTreeSet::new();
    for path in writable {
        // `components/<name>/...` is the only shape that names a component.
        let Some(rest) = path.strip_prefix("components/") else { continue };
        let Some(component) = rest.split('/').next() else { continue };
        let Some(surface) = catalog.surface(component) else { continue };
        tags.insert(component.to_string());
        tags.extend(surface.imports.iter().cloned());
    }
    tags.into_iter().collect()
}

/// The directories built components normally live in.
pub fn default_dirs(repo_root: &Path) -> Vec<PathBuf> {
    ["wasm32-wasip2/release", "wasm32-wasip2/debug", "wasm32-wasip1/release", "wasm32-wasip1/debug"]
        .iter()
        .map(|d| repo_root.join("components/target").join(d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repo_root;

    fn catalog() -> Catalog {
        Catalog::scan(&default_dirs(&repo_root()))
    }

    #[test]
    fn a_surface_comes_from_the_binary() {
        let catalog = catalog();
        let Some(surface) = catalog.surface("record-store") else {
            eprintln!("SKIPPED: nothing built — run `cargo xtask build --force`");
            return;
        };
        assert!(
            surface.exports.iter().any(|e| e.starts_with("records:store/")),
            "record-store exports its own package: {:?}",
            surface.exports
        );
        assert!(
            surface.host_imports.iter().all(|i| HOST_NAMESPACES.contains(&namespace(i))),
            "a host import is only ever wasi/wasmcloud/comp: {:?}",
            surface.host_imports
        );
    }

    #[test]
    fn composition_is_recursive_and_leaves_nothing_dangling() {
        let catalog = catalog();
        if catalog.surface("vet-domain").is_none() {
            eprintln!("SKIPPED: nothing built — run `cargo xtask build --force`");
            return;
        }
        // The hardest case: 22 capabilities, one of which (`auth-guard`) has capabilities of its own.
        // Asserts on the RESULT rather than the plug list.
        let bytes = compose("vet-domain", &catalog).expect("composed");
        let after = surface(&bytes).expect("the result is a component");
        // An import that something in the catalogue EXPORTS and that is still there afterwards is a
        // capability the composition failed to bind. (Imports nothing exports are types-only.)
        let dangling: Vec<&String> =
            after.imports.iter().filter(|i| catalog.exporter(i).is_some()).collect();
        assert!(dangling.is_empty(), "composition left capabilities unsatisfied: {dangling:?}");
    }

    #[test]
    fn a_flat_chain_would_leave_them_dangling() {
        // Why `compose` recurses, pinned: composing vet-domain the flat way still validates but
        // leaves the plugs' own imports hoisted.
        let catalog = catalog();
        if catalog.surface("vet-domain").is_none() {
            eprintln!("SKIPPED: nothing built — run `cargo xtask build --force`");
            return;
        }
        let plugs: Vec<(String, Vec<u8>)> = wiring("vet-domain", &catalog)
            .unwrap()
            .plugs
            .into_iter()
            .map(|p| (p.clone(), catalog.bytes(&p).unwrap().to_vec()))
            .collect();
        let flat = plug_together("vet-domain", catalog.bytes("vet-domain").unwrap(), &plugs)
            .expect("a flat chain composes fine — that is the problem");
        let after = surface(&flat).expect("the result is a component");
        let dangling: Vec<&String> =
            after.imports.iter().filter(|i| catalog.exporter(i).is_some()).collect();
        assert!(
            !dangling.is_empty(),
            "a flat chain left nothing dangling, so recursion buys nothing and \
             `compose` can be simplified"
        );
    }

    #[test]
    fn a_missing_capability_is_reported_rather_than_guessed() {
        let mut catalog = Catalog::default();
        catalog.surfaces.insert(
            "lonely".into(),
            Surface { imports: ["nobody:home/iface@0.1.0".into()].into(), ..Default::default() },
        );
        let wiring = wiring("lonely", &catalog).unwrap();
        assert!(wiring.plugs.is_empty());
        assert_eq!(wiring.missing, vec![("lonely".into(), "nobody:home/iface@0.1.0".into())]);
    }
}
