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

use std::collections::{BTreeMap, BTreeSet};

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

/// An application, and the component it is composed from.
///
/// Discovered from the `Justfile`, which is now able to answer this: every
/// showcase used to spell out its own `wac plug` chain, so "what is this app made
/// of" was a hand-written list that drifted. Since the chains became `_derive
/// <component> <artifact>` calls, the recipe states only the ROOT — and the rest
/// is derived from the artifact, which means this graph cannot disagree with what
/// actually gets composed.
struct App {
    name: String,
    root: String,
    artifact: String,
}

fn apps(root_dir: &std::path::Path) -> Vec<App> {
    let Ok(justfile) = std::fs::read_to_string(root_dir.join("Justfile")) else {
        return Vec::new();
    };
    // The just variables, so `{{vet_composed}}` becomes a path.
    let mut vars: BTreeMap<&str, &str> = BTreeMap::new();
    for line in justfile.lines() {
        if let Some((name, rest)) = line.split_once(":=") {
            let name = name.trim();
            let value = rest.trim().trim_matches('"');
            if !name.contains(' ') && !value.is_empty() {
                vars.insert(name, value);
            }
        }
    }

    let mut out = Vec::new();
    let mut recipe = String::new();
    for line in justfile.lines() {
        if !line.starts_with([' ', '\t']) && line.contains(':') {
            recipe = line.split([':', ' ']).next().unwrap_or("").to_string();
        }
        let Some(rest) = line.trim().strip_prefix("@just _derive ") else { continue };
        let mut parts = rest.split_whitespace();
        let (Some(component), Some(artifact)) = (parts.next(), parts.next()) else { continue };
        let artifact = artifact.trim_start_matches("{{").trim_end_matches("}}");
        let artifact = vars.get(artifact).copied().unwrap_or(artifact);
        // `compose-vet` is the app `vet`; the bare `compose` recipe builds a plug
        // for other apps rather than an app of its own.
        let name = recipe.strip_prefix("compose-").unwrap_or(&recipe).to_string();
        out.push(App {
            name: if name == "compose" { component.to_string() } else { name },
            root: component.to_string(),
            artifact: artifact.rsplit('/').next().unwrap_or(artifact).to_string(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.root.cmp(&b.root)));
    out.dedup_by(|a, b| a.root == b.root && a.name == b.name);
    out
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
        "json" => println!("{}", json(&catalog, &by_iface, &orphans, &apps(&root))),
        "mermaid" => println!("{}", mermaid(&by_iface, args.diagram_top)),
        "md" => println!(
            "{}",
            markdown(&catalog, &by_iface, &orphans, args.diagram_top, &apps(&root))
        ),
        other => return Err(format!("unknown format {other:?} — md, json or mermaid")),
    }
    Ok(())
}

/// The same graph, for something other than a reader.
///
/// ADR-0089's first slice is a capability search — "what already provides this?"
/// — and it needs the graph as data rather than as a table. The application layer
/// is in here for the same reason: "how many apps carry this" is the number a
/// promotion or a deletion should be checked against, and no tool can read it out
/// of a markdown file.
fn json(
    catalog: &Catalog,
    by_iface: &BTreeMap<String, (String, Vec<String>)>,
    orphans: &[(String, String)],
    apps: &[App],
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
    out.push_str("\n  ],\n  \"apps\": [\n");
    let mut used_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (i, app) in apps.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        let parts = catalog.closure(&app.root);
        for p in &parts {
            used_by.entry(p.clone()).or_default().push(app.name.clone());
        }
        let list = parts.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ");
        out.push_str(&format!(
            "    {{ \"app\": \"{}\", \"root\": \"{}\", \"artifact\": \"{}\", \"composes\": [{list}] }}",
            app.name, app.root, app.artifact
        ));
    }
    out.push_str("\n  ],\n  \"component_in_apps\": [\n");
    let mut ranked: Vec<_> = used_by.into_iter().collect();
    ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    for (i, (component, apps_using)) in ranked.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        let list = apps_using.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(", ");
        out.push_str(&format!(
            "    {{ \"component\": \"{component}\", \"app_count\": {}, \"apps\": [{list}] }}",
            apps_using.len()
        ));
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

/// Apps on the left, the capabilities more than one of them carries on the right.
///
/// Only the shared ones: an app's private component says nothing about coupling,
/// and drawing all 150 produces a picture nobody can read.
fn app_mermaid(
    made_of: &[(&App, Vec<String>)],
    used_by: &BTreeMap<String, BTreeSet<String>>,
) -> String {
    let shared: BTreeSet<&String> =
        used_by.iter().filter(|(_, a)| a.len() >= 6).map(|(c, _)| c).collect();
    let mut out = String::from("```mermaid\ngraph LR\n");
    for (app, parts) in made_of.iter().take(14) {
        let id = format!("app_{}", app.name.replace('-', "_"));
        out.push_str(&format!("  {id}[\"{}\"]\n", app.name));
        for part in parts.iter().filter(|p| shared.contains(p)) {
            out.push_str(&format!("  {id} --> {}([{}])\n", part.replace('-', "_"), part));
        }
    }
    out.push_str("```\n");
    out
}

fn markdown(
    catalog: &Catalog,
    by_iface: &BTreeMap<String, (String, Vec<String>)>,
    orphans: &[(String, String)],
    top: usize,
    apps: &[App],
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
         actually do, the way a hand-maintained dependency list does.\n\n\
         Three layers: an INTERFACE is provided by one component and imported by \
         several; a COMPONENT is composed into one or more applications; an \
         APPLICATION is a root component plus everything `wac` pulls in behind it. \
         The three answer different questions, and the second is the one that was \
         missing — `rate-limiter` has almost no direct consumers and is inside \
         twenty-two apps, because it rides in as a plug of `auth-guard`.\n\n",
    );
    s.push_str(&format!(
        "**{} components, {} interfaces with a provider and at least one consumer, {} \
         import edges, {} interfaces exported but unconsumed in-tree, {} applications \
         composed from them.**\n\n",
        catalog.len(),
        by_iface.len(),
        total_edges,
        orphans.len(),
        apps.len()
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
    // --- the application layer ------------------------------------------------
    let mut used_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut made_of: Vec<(&App, Vec<String>)> = Vec::new();
    for app in apps {
        let parts = catalog.closure(&app.root);
        for part in &parts {
            used_by.entry(part.clone()).or_default().insert(app.name.clone());
        }
        made_of.push((app, parts));
    }

    s.push_str("\n\n## Which apps is this component inside?\n\n");
    s.push_str(
        "The blast radius, and it is not the consumer count above. That column counts \
         components that IMPORT an interface; this one counts APPLICATIONS that carry \
         the component once it is composed, plugs of plugs included. A capability with \
         two direct consumers can still end up inside twenty apps.\n\nThis is the \
         number to look at before changing a component, and before deleting one — \
         something nothing imports directly may still be composed into a dozen \
         artifacts.\n\n",
    );
    let mut ranked_components: Vec<_> = used_by.iter().collect();
    ranked_components.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    s.push_str("| apps | component | which |\n| --: | --- | --- |\n");
    for (component, apps_using) in &ranked_components {
        s.push_str(&format!(
            "| {} | `{component}` | {} |\n",
            apps_using.len(),
            apps_using.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", ")
        ));
    }

    s.push_str("\n## What is each app made of?\n\n");
    s.push_str(
        "Read off the artifact, not off a build file. Every showcase used to name its \
         own plug list by hand and most were wrong — the vet clinic claimed five and \
         composes twenty-two. A recipe now states only the ROOT; everything after it \
         is derived, so this table cannot disagree with what `just compose-*` \
         actually produces (ADR-0087).\n\n",
    );
    s.push_str("| app | root component | composes | artifact |\n| --- | --- | --: | --- |\n");
    for (app, parts) in &made_of {
        s.push_str(&format!(
            "| **{}** | `{}` | {} | `{}` |\n",
            app.name,
            app.root,
            parts.len(),
            app.artifact
        ));
    }

    s.push_str("\n### The apps, and the capabilities they share\n\n");
    s.push_str(&app_mermaid(&made_of, &used_by));

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
