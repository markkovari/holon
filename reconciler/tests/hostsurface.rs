//! What a component may ask of its HOST, pinned.
//!
//! ADR-0023 says isolation is a linker boundary: a component reaches exactly what its import section
//! names. That is a guarantee only if somebody notices when the boundary moves. `wit/SURFACES.md`
//! snapshots the repository's own interfaces; this is the other half, the ones it does not define.
//!
//! **The floor is fourteen, and it is not a leak.** 205 of 207 components import the SAME fourteen WASI
//! interfaces; `resilience` and `shaper` import nothing, proving the linker drops what nothing reaches.
//! They come from `std` (the allocator's OOM path and panic handler need `wasi:cli`). `panic = "abort"`
//! does not remove them; only `no_std` would. So the floor is a fact to KNOW, and what is worth
//! guarding is the line above it.
//!
//! A component that starts importing `wasi:sockets` or `wasi:filesystem` has been handed an unreviewed
//! capability that reads as a routine dependency bump in the diff. Here it is a failing test naming the
//! component and interface, so widening the list is a deliberate edit with a reason.

use std::collections::{BTreeMap, BTreeSet};

use holon_fleet::fleet::repo_root;
use holon_fleet::plug::Catalog;

/// Every host interface a component in this repository is allowed to import. The first group is the
/// `std` floor; the rest are asked for on purpose, the count beside each being what it was when the
/// line was written (a number that quietly only goes up is what this makes loud).
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
    "wasi:clocks/types", // p3's duration type, pulled in by http/types and monotonic-clock
    "wasi:io/error",
    "wasi:io/poll",
    "wasi:io/streams",
    "wasi:blobstore/blobstore",   //   1 — blob-store, the only one
    "wasi:blobstore/container",   //   1
    "wasi:blobstore/types",       //   1
    "wasi:clocks/wall-clock",     //  67 — a timestamp, not a duration
    "wasi:clocks/system-clock",   //  72 — the same timestamp, for p3 components (ADR-0103)
    "wasi:config/store",          //  37
    "wasi:http/outgoing-handler", // 18 — the components that call out
    "wasi:http/client",           //  6 — the same, for p3 components
    "wasi:http/types",            // 109 — every component that serves
    "wasi:keyvalue/atomics",      //   5
    "wasi:keyvalue/batch",        //   2
    "wasi:keyvalue/store",        //  57
    "wasi:random/insecure-seed",  // 10
    "wasi:random/random",         //  15
    // This repository's own host interfaces. `comp:` is in HOST_NAMESPACES because a host provides it,
    // so it belongs here, and is a capability grant like any other.
    "comp:secrets/reader", //   8
    "comp:store/cas",      //   5
    // The one interface a wasmCloud release host provides beyond standard WASI.
    "wasmcloud:messaging/types", // 1 — event-pusher
];

/// An import is `wasi:http/types@0.2.0`; the version is upstream's business, so the claim is about
/// the INTERFACE.
fn without_version(iface: &str) -> &str {
    iface.split('@').next().unwrap_or(iface)
}

#[test]
fn no_component_reaches_for_a_host_capability_nobody_granted() {
    let root = repo_root();
    let dir = root.join("components/target/wasm32-wasip2/release");
    let catalog = Catalog::scan(std::slice::from_ref(&dir));
    assert!(
        catalog.names().count() > 100,
        "only {} components in {} — run `cargo xtask build --force` first",
        catalog.names().count(),
        dir.display()
    );

    let allowed: BTreeSet<&str> = ALLOWED.iter().copied().collect();
    // Grouped by interface, not component: one new import usually arrives in several at once.
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
                let more = if who.len() > 6 {
                    format!(" (+{} more)", who.len() - 6)
                } else {
                    String::new()
                };
                let names = who.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
                format!("  {iface}\n    wanted by: {names}{more}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    println!(
        "  {} components, {} distinct host interfaces:",
        catalog.names().count(),
        counts.len()
    );
    for (iface, n) in &counts {
        println!("    {n:>4}  {iface}");
    }
}
