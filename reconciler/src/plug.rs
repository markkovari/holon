//! Composition as a library call: wrap `wac`, do not run it.
//!
//! ## What this replaces
//!
//! 59 hand-written `wac plug … --plug … --plug …` chains in the `Justfile`, one
//! per showcase. The recipe for assembling an app lived in a build file rather
//! than in the app, so a component the loop built could not be composed, run or
//! deployed until a person edited that file. A substrate whose thesis is
//! composition should not need a human to spell the composition out — and it does
//! not have to, because a component already states its imports and every
//! capability already states what it exports. That is a complete wiring diagram.
//!
//! ## Why a library rather than a driver around the CLI
//!
//! `wac` is a Rust crate before it is a command. Shelling out to it means the
//! answer arrives as text to be parsed, every caller needs the binary on `PATH`,
//! and the interesting parts — which plug satisfies which import, what is still
//! unsatisfied, whether the graph is even buildable — have to be recovered from
//! stderr. Calling `wac_graph` directly gives all of it as values, and the loop
//! can compose a candidate in-process without spawning anything.
//!
//! Two things this gets right that a shell version of it got wrong, both found by
//! being wrong first:
//!
//! * **A flat plug chain is not a composition.** `wac plug root --plug a --plug b`
//!   satisfies the ROOT's imports and hoists each plug's own imports into the
//!   result. The "composed" vet clinic still imported `audit:log`,
//!   `ratelimit:guard` and `llm:inference`, and `wasm-tools validate` was happy
//!   with it. That is why the `Justfile` pre-composes `auth-guard` as a separate
//!   step; [`compose`] recurses instead, so every plug goes in whole.
//! * **Resolution is per-interface, not per-package.** `cache-backing` exports
//!   `cache:store/sink` and `cache:store/source` but not `cache:store/cache`, so
//!   a package-level match reports "satisfied" for an import that then dangles.
//!
//! ## What it does not do
//!
//! Decide whether a plug's TYPES fit — it matches interface names and lets
//! `wac_graph::plug` refuse what does not fit. `components/wit-reflect` exposes
//! `wac`'s own `SubtypeChecker` through `satisfies` for callers that need the
//! answer before composing (a UI drawing an edge); here the composition itself is
//! the check, and it happens immediately.
//!
//! `wit-reflect` wraps the same crate for the component side, where a sandboxed
//! app inspects and composes without a host. This is the native side, for the
//! loop and its gates — the same split as `checks-runner` (component) and
//! `comp-checks` (native).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Namespaces only a HOST can satisfy. An import from one of these is a runtime
/// capability, never a composition edge — and telling the two apart is the whole
/// difference between "this composition is incomplete" and "this is normal".
const HOST_NAMESPACES: &[&str] = &["wasi", "wasmcloud", "comp"];

/// What a component imports and exports, read out of the binary.
///
/// Out of the BINARY, not out of `components/*/wit/`: `auth-guard` has no wit
/// directory at all (it targets a world in the shared root `wit/`), a source
/// directory can declare a package it does not export, and — the one that matters
/// most here — the compiler DROPS an import nothing calls. What survives to the
/// artifact is what the component actually uses.
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
    // Only the OUTER component's sections describe THIS component; a composed
    // artifact carries its plugs' sections too, and counting those would report
    // every capability's imports as if they were still unsatisfied.
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
    /// Read every component in `dirs`. Earlier directories win, so a gate that
    /// rebuilt one crate puts its own output first and lets the rest resolve
    /// against what `just build` already produced.
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
    /// `(component, interface)` — an import nothing built can satisfy. Not an
    /// error on its own: a types-only interface from a component's own package
    /// has nothing to implement and dangles through a perfectly good composition.
    pub missing: Vec<(String, String)>,
}

/// Who plugs into `name`, from what it imports.
pub fn wiring(name: &str, catalog: &Catalog) -> Result<Wiring, String> {
    let surface = catalog.surface(name).ok_or_else(|| format!("no built component '{name}'"))?;
    let mut out = Wiring::default();
    for import in &surface.imports {
        match catalog.exporter(import) {
            // A component may re-export what it imports; plugging it into itself
            // is a cycle, and `wac` says so less clearly than this does.
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

/// Compose `name` with everything it imports, recursively.
///
/// Each plug is composed BEFORE it is plugged, because a plug that is not itself
/// whole leaves its own imports hoisted into the result.
pub fn compose(name: &str, catalog: &Catalog) -> Result<Vec<u8>, String> {
    compose_inner(name, catalog, &mut Vec::new())
}

fn compose_inner(name: &str, catalog: &Catalog, stack: &mut Vec<String>) -> Result<Vec<u8>, String> {
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

/// Compose and write the result, keyed by the content that went into it.
///
/// Content-addressed for two reasons: a gate that runs twenty times composes
/// once, and the artifact outlives the run that made it — which is what makes a
/// composed app something to deploy or push to the catalogue rather than a
/// temporary file in `/tmp`.
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
    std::fs::write(&out, bytes).map_err(|e| e.to_string())?;
    Ok(out)
}

/// The directories built components normally live in.
pub fn default_dirs(repo_root: &Path) -> Vec<PathBuf> {
    ["wasm32-wasip2/release", "wasm32-wasip2/debug", "wasm32-wasip1/debug"]
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
            eprintln!("SKIPPED: nothing built — run `just build`");
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
            eprintln!("SKIPPED: nothing built — run `just build`");
            return;
        }
        // The hardest case in the repository, and the one the hand-written
        // `just compose-vet` gets wrong: 22 capabilities, one of which
        // (`auth-guard`) has capabilities of its own. A flat chain leaves those
        // inner ones dangling, which is why this asserts on the RESULT rather
        // than on the plug list.
        let bytes = compose("vet-domain", &catalog).expect("composed");
        let after = surface(&bytes).expect("the result is a component");
        // The property, stated the only way that has teeth: an import that
        // something in the catalogue EXPORTS and that is still there afterwards is
        // a capability the composition failed to bind. (Imports nothing exports
        // are types-only interfaces — `audit-log` imports `audit:log/types` while
        // exporting `audit:log/query` — and there is nothing to plug into them.)
        let dangling: Vec<&String> =
            after.imports.iter().filter(|i| catalog.exporter(i).is_some()).collect();
        assert!(dangling.is_empty(), "composition left capabilities unsatisfied: {dangling:?}");
    }

    #[test]
    fn a_flat_chain_would_leave_them_dangling() {
        // Why `compose` recurses, pinned. `wac plug root --plug a --plug b`
        // satisfies the ROOT's imports and hoists each plug's own imports into
        // the result — and the result still validates, so nothing complains. This
        // composes vet-domain the flat way and shows the difference.
        let catalog = catalog();
        if catalog.surface("vet-domain").is_none() {
            eprintln!("SKIPPED: nothing built — run `just build`");
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
