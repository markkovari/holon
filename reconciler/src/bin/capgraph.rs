//! `comp-capgraph` — what is using what, across every built component.
//!
//! The question this answers is not "what does this app depend on" — a build file
//! can tell you that, badly. It is the reverse, and it is the one nobody can
//! answer from memory once a repository has 150 components and 93 exported
//! interfaces:
//!
//!   * **May I change this interface?** `records:store/store` has 37 consumers, so
//!     no, not in place. `slug:generate/generator` has one. Same repository, two
//!     completely different answers, and nothing surfaced the difference before.
//!   * **Does this capability already exist?** The first question a goal should
//!     ask before generating an implementation (ADR-0089), and the pool is far
//!     past the size where a person can hold the answer.
//!   * **What breaks if this component goes?** Its export list, crossed with who
//!     imports those interfaces.
//!
//! Derived from the BUILT artifacts every time, never maintained by hand. A
//! hand-written dependency list is wrong the first time somebody adds an import
//! and forgets the list — and a component's real imports are in the binary, so
//! there is no reason to keep a second copy that can disagree.
//!
//!   comp-capgraph                 → the markdown report (docs/CAPABILITY-GRAPH.md)
//!   comp-capgraph --format json   → the same graph, for tooling
//!   comp-capgraph --format mermaid → just the diagram

use std::collections::BTreeMap;

use clap::Parser;
use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::{default_dirs, Catalog};

#[derive(Parser)]
#[command(name = "comp-capgraph", about = "The capability graph: who imports what from whom")]
struct Args {
    /// `md` (default), `json`, or `mermaid`.
    #[arg(long, default_value = "md")]
    format: String,

    /// How many interfaces the diagram shows, most-consumed first. The full graph
    /// has 93 of them and renders as a hairball; the tables below it are complete.
    #[arg(long, default_value_t = 12)]
    diagram_top: usize,
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    let root = repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.is_empty() {
        return Err("nothing is built — run `just build` first".into());
    }

    let edges = catalog.edges();
    // interface -> (provider, consumers)
    let mut by_iface: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for (consumer, iface, provider) in &edges {
        by_iface
            .entry(iface.clone())
            .or_insert_with(|| (provider.clone(), Vec::new()))
            .1
            .push(consumer.clone());
    }
    let orphans = catalog.orphan_exports();

    match args.format.as_str() {
        "json" => println!("{}", json(&by_iface, &orphans)),
        "mermaid" => println!("{}", mermaid(&by_iface, args.diagram_top)),
        "md" => println!("{}", markdown(&catalog, &by_iface, &orphans, args.diagram_top)),
        other => return Err(format!("unknown format {other:?} — md, json or mermaid")),
    }
    Ok(())
}

fn json(
    by_iface: &BTreeMap<String, (String, Vec<String>)>,
    orphans: &[(String, String)],
) -> String {
    let mut out = String::from("{\n  \"interfaces\": [\n");
    let mut first = true;
    for (iface, (provider, consumers)) in by_iface {
        if !first {
            out.push_str(",\n");
        }
        first = false;
        let list =
            consumers.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
        out.push_str(&format!(
            "    {{ \"interface\": \"{iface}\", \"provider\": \"{provider}\", \
             \"consumers\": [{list}], \"consumer_count\": {} }}",
            consumers.len()
        ));
    }
    out.push_str("\n  ],\n  \"exported_but_unconsumed\": [\n");
    for (i, (owner, iface)) in orphans.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str(&format!("    {{ \"component\": \"{owner}\", \"interface\": \"{iface}\" }}"));
    }
    out.push_str("\n  ]\n}");
    out
}

/// Mermaid renders natively on GitHub, so the diagram is readable in the doc
/// rather than requiring a tool nobody has installed.
fn mermaid(by_iface: &BTreeMap<String, (String, Vec<String>)>, top: usize) -> String {
    let mut ranked: Vec<_> = by_iface.iter().collect();
    ranked.sort_by(|a, b| b.1 .1.len().cmp(&a.1 .1.len()).then(a.0.cmp(b.0)));

    let mut out = String::from("```mermaid\ngraph LR\n");
    for (iface, (provider, consumers)) in ranked.iter().take(top) {
        let short = iface.split('@').next().unwrap_or(iface);
        let id = short.replace([':', '/', '-', '.'], "_");
        out.push_str(&format!("  {id}([\"{short}\"])\n"));
        out.push_str(&format!("  {}[{}] --> {id}\n", provider.replace('-', "_"), provider));
        // Naming 37 consumers makes an unreadable picture; the count is the point,
        // and the table below has every name.
        if consumers.len() > 4 {
            out.push_str(&format!("  {id} --> many_{id}[\"{} consumers\"]\n", consumers.len()));
        } else {
            for c in consumers.iter() {
                out.push_str(&format!("  {id} --> {}[{}]\n", c.replace('-', "_"), c));
            }
        }
    }
    out.push_str("```");
    out
}

fn markdown(
    catalog: &Catalog,
    by_iface: &BTreeMap<String, (String, Vec<String>)>,
    orphans: &[(String, String)],
    top: usize,
) -> String {
    let mut ranked: Vec<_> = by_iface.iter().collect();
    ranked.sort_by(|a, b| b.1 .1.len().cmp(&a.1 .1.len()).then(a.0.cmp(b.0)));
    let total_edges: usize = by_iface.values().map(|(_, c)| c.len()).sum();

    let mut s = String::new();
    s.push_str(
        "# The capability graph\n\n\
         Generated by `comp-capgraph` — do not edit by hand. Regenerate with `just capgraph`.\n\n\
         Derived from the BUILT artifacts, not from `components/*/wit/` and not from the\n\
         `Justfile`. A component's real imports are in its binary — the compiler drops\n\
         the ones nothing calls — so this cannot drift from what the components\n\
         actually do, the way a hand-maintained dependency list does.\n\n",
    );
    s.push_str(&format!(
        "**{} components, {} interfaces with a provider and at least one consumer, {} \
         import edges, {} interfaces exported but unconsumed in-tree.**\n\n",
        catalog.len(),
        by_iface.len(),
        total_edges,
        orphans.len()
    ));
    s.push_str(
        "## Can I change this interface?\n\n\
         The number in the first column is the answer. One consumer means an edit; \
         thirty-seven means a migration, and no gate in this repository runs those \
         thirty-seven apps. An interface with many consumers is frozen in practice \
         whatever its version number says, and extending it means a NEW interface or \
         a new version with both live — see [ADR-0089](adr/0089-capability-accumulation.md).\n\n",
    );
    s.push_str("| consumers | interface | provider | who imports it |\n");
    s.push_str("| --: | --- | --- | --- |\n");
    for (iface, (provider, consumers)) in &ranked {
        let short = iface.split('@').next().unwrap_or(iface);
        s.push_str(&format!(
            "| {} | `{short}` | `{provider}` | {} |\n",
            consumers.len(),
            consumers.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ")
        ));
    }

    s.push_str("\n## The load-bearing few\n\n");
    s.push_str(&mermaid(by_iface, top));
    s.push_str("\n\n## Exported, and nothing in this repository imports it\n\n");
    s.push_str(
        "Not a finding. A capability library is allowed to be ahead of its callers, \
         and several of these are providers meant to be swapped in at deploy time. \
         It is here because \"nobody depends on this yet\" is exactly the window in \
         which an interface can still be changed freely.\n\n",
    );
    s.push_str("| component | interface |\n| --- | --- |\n");
    for (owner, iface) in orphans {
        let short = iface.split('@').next().unwrap_or(iface);
        s.push_str(&format!("| `{owner}` | `{short}` |\n"));
    }
    s
}
