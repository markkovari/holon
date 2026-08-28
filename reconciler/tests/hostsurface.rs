//! What a component may ask of its HOST, pinned.
//!
//! ADR-0023 says isolation is a linker boundary: a component can reach exactly
//! what its import section names and nothing else. That is only a guarantee if
//! somebody notices when the boundary moves — and until this test, nothing did.
//! `wit/SURFACES.md` snapshots what the repository's own interfaces look like;
//! this is the other half, the interfaces it does not define and cannot change.
//!
//! ## The floor is fourteen, and it is not a leak
//!
//! Measured, not assumed: 205 of 207 built components import the SAME fourteen
//! WASI interfaces, whatever they do. `resilience` and `shaper` import nothing at
//! all, which is the useful control — it means the linker really does drop what
//! nothing reaches, so those fourteen are reached. They come from `std`: the
//! allocator's out-of-memory path and the panic handler need somewhere to write
//! and a way to stop, and on `wasm32-wasip2` that is `wasi:cli`.
//!
//! `panic = "abort"` does not remove them (tried). Nothing short of `no_std`
//! would, and 205 components using `String` and `serde` are not going `no_std`.
//! So the floor is a fact to KNOW rather than a defect to fix, and what is worth
//! guarding is the line above it.
//!
//! ## What this actually catches
//!
//! A component that starts importing `wasi:sockets` or `wasi:filesystem` has been
//! handed a capability nobody reviewed, and it will read as a routine dependency
//! bump in the diff. Here it reads as a failing test that names the component and
//! the interface. Widening the list is then a deliberate edit with a reason next
//! to it, which is the whole point.

use std::collections::{BTreeMap, BTreeSet};

use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::Catalog;

/// Every host interface a component in this repository is allowed to import.
///
/// The first group is the `std` floor described above — present in all but the two
/// components that import nothing. The rest are asked for on purpose, and the
/// count beside each is what it was when the line was written: a number that only
/// goes up quietly is the thing this test exists to make loud.
const ALLOWED: &[&str] = &[
    // The std floor. Nothing in this group is called by any component's own code.
    "wasi:cli/environment",
    "wasi:cli/exit",
    "wasi:cli/stderr",
    "wasi:cli/stdin",
    "wasi:cli/stdout",
    "wasi:cli/terminal-input",
    "wasi:cli/terminal-output",
    "wasi:cli/terminal-stderr",
    "wasi:cli/terminal-stdin",
    "wasi:cli/terminal-stdout",
    "wasi:clocks/monotonic-clock",
    "wasi:io/error",
    "wasi:io/poll",
    "wasi:io/streams",
    // Asked for deliberately.
    "wasi:blobstore/blobstore", //   1 — blob-store, the only one
    "wasi:blobstore/container", //   1
    "wasi:blobstore/types",     //   1
    "wasi:clocks/wall-clock",   //  67 — a timestamp, not a duration
    "wasi:config/store",        //  37
    "wasi:http/outgoing-handler", // 18 — the components that call out
    "wasi:http/types",          // 109 — every component that serves
    "wasi:keyvalue/atomics",    //   5
    "wasi:keyvalue/batch",      //   2
    "wasi:keyvalue/store",      //  57
    "wasi:random/insecure-seed", // 10
    "wasi:random/random",       //  15
    // This repository's own host interfaces. `comp:` is in HOST_NAMESPACES
    // because a host provides it, not another component — so it belongs here
    // rather than in the composable half, and it is a capability grant like any
    // other: `comp:secrets/reader` reads secrets.
    "comp:secrets/reader",      //   8
    "comp:store/cas",           //   5
    // The one interface a wasmCloud release host provides beyond standard WASI.
    "wasmcloud:messaging/types", // 1 — event-pusher
];

/// An import is `wasi:http/types@0.2.0`; the version is upstream's business and
/// moves on its own schedule, so the claim is about the INTERFACE.
fn without_version(iface: &str) -> &str {
    iface.split('@').next().unwrap_or(iface)
}

#[test]
fn no_component_reaches_for_a_host_capability_nobody_granted() {
    let root = repo_root();
    let dir = root.join("components/target/wasm32-wasip2/release");
    let catalog = Catalog::scan(&[dir.clone()]);
    assert!(
        catalog.names().count() > 100,
        "only {} components in {} — run `just build` first",
        catalog.names().count(),
        dir.display()
    );

    let allowed: BTreeSet<&str> = ALLOWED.iter().copied().collect();
    // Grouped by interface rather than by component: one new import usually
    // arrives in several components at once, and a list of forty lines that all
    // say the same thing buries which interface it is.
    let mut unexpected: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();

    for name in catalog.names().map(str::to_string).collect::<Vec<_>>() {
        let Some(surface) = catalog.surface(&name) else { continue };
        for import in &surface.host_imports {
            let iface = without_version(import);
            if let Some(known) = allowed.get(iface) {
                *counts.entry(known).or_default() += 1;
            } else {
                unexpected.entry(iface.to_string()).or_default().push(name.clone());
            }
        }
    }

    assert!(
        unexpected.is_empty(),
        "component(s) import a host interface that is not in ALLOWED.\n\n\
         This is a capability grant, not a dependency bump: whatever imports it can \n\
         reach it at run time. Add it to ALLOWED with a reason if that is intended.\n\n{}",
        unexpected
            .iter()
            .map(|(iface, who)| {
                let more =
                    if who.len() > 6 { format!(" (+{} more)", who.len() - 6) } else { String::new() };
                let names = who.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
                format!("  {iface}\n    wanted by: {names}{more}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    println!("  {} components, {} distinct host interfaces:", catalog.names().count(), counts.len());
    for (iface, n) in &counts {
        println!("    {n:>4}  {iface}");
    }
}
