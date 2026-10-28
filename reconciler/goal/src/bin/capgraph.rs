//! Who imports what, across every built component; derived from the binaries, never by hand.

use std::collections::{BTreeMap, BTreeSet};

use clap::Parser;
use holon_fleet::fleet::repo_root;
use holon_fleet::plug::{default_dirs, Catalog};

#[derive(Parser)]
#[command(name = "comp-capgraph", about = "The capability graph: who imports what from whom")]
struct Args {
    /// `json` or `surql` (default).
    #[arg(long, default_value = "surql")]
    format: String,

    /// The generation stamped on a `surql` projection (ADR-0091); defaults to the wall clock.
    #[arg(long)]
    gen: Option<u64>,

    /// Ask the catalogue "do we already have something for this?" instead of printing the graph.
    #[arg(long)]
    find: Option<String>,

    /// Report every interface exported by more than one component, and stop.
    #[arg(long)]
    twins: bool,
}

use holon_metadata::app::App;

fn main() -> Result<(), String> {
    let args = Args::parse();
    let root = repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.is_empty() {
        return Err("nothing is built — run `cargo xtask build --force` first".into());
    }

    let edges = catalog.edges();
    let mut by_iface: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for (consumer, iface, provider) in &edges {
        by_iface
            .entry(iface.clone())
            .or_insert_with(|| (provider.clone(), Vec::new()))
            .1
            .push(consumer.clone());
    }
    let orphans = catalog.orphan_exports();

    if args.twins {
        let found = holon_goal::capsearch::twins(&catalog);
        if found.is_empty() {
            println!(
                "no interface in the catalogue is exported by more than one component.\n\n\
                 That is a finding, not a blank: {} components, and nothing was built twice.",
                catalog.names().count()
            );
            return Ok(());
        }
        println!(
            "{} interface(s) exported by more than one component, most-overlapping first.\n\n\
             A pair is not automatically a mistake — a reference and its mock share an \n\
             interface on purpose. What to look at is `shared`: two components that agree \n\
             on ONE interface may be alternatives, two that agree on their whole surface \n\
             are the same component written twice.\n",
            found.len()
        );
        for t in &found {
            // A `wasi:` interface everybody exports is the system's shape, not duplication.
            if t.interface.starts_with("wasi:") {
                println!(
                    "  {:<34} {} components export it — a standard entry point, not a duplicate",
                    t.interface,
                    t.components.len()
                );
                continue;
            }
            println!(
                "  {:<34} {}\n      overlap {:.0}%  shared: {}",
                t.interface,
                t.components.join(", "),
                t.overlap * 100.0,
                if t.shared.len() > 1 { t.shared.join(", ") } else { "(only this one)".into() },
            );
        }
        // `comp-plug` resolves an interface to one exporter by scan order, silently.
        println!(
            "\nNote: `comp-plug` picks a single exporter per interface by scan order. For \
             each line above,\nwhichever component sorts first is what every composition \
             silently gets."
        );
        return Ok(());
    }

    if let Some(query) = &args.find {
        let mut apps_of: BTreeMap<String, usize> = BTreeMap::new();
        for app in holon_metadata::app::discover_apps(&root) {
            for part in catalog.closure(&app.root) {
                *apps_of.entry(part).or_default() += 1;
            }
        }
        let pool = holon_goal::capsearch::capabilities(&root, &catalog, &apps_of);
        let hits = holon_goal::capsearch::find(query, &pool);
        if hits.is_empty() {
            println!(
                "nothing in the catalogue matches {query:?}.\n\nThat is an answer: build \
                 it, and if it generalises, promote it (ADR-0089)."
            );
            return Ok(());
        }
        println!("{} of {} capabilities match {query:?}:\n", hits.len(), pool.len());
        for m in hits.iter().take(8) {
            println!(
                "  {:<20} {:>2} app(s)  matched on {}\n      {}\n      exports {}",
                m.capability.name,
                m.capability.apps,
                m.because.join(", "),
                if m.capability.description.is_empty() {
                    "(no `//!` description on the component)"
                } else {
                    &m.capability.description
                },
                m.capability
                    .exports
                    .iter()
                    .map(|e| e.split('@').next().unwrap_or(e))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        return Ok(());
    }

    match args.format.as_str() {
        "json" => println!(
            "{}",
            json(&catalog, &by_iface, &orphans, &holon_metadata::app::discover_apps(&root))
        ),
        "surql" => {
            let generation = args.gen.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
            println!("{}", surql(&catalog, &holon_metadata::app::discover_apps(&root), generation));
        }
        other => return Err(format!("unknown format {other:?} — json or surql")),
    }
    Ok(())
}

/// "How many apps carry this" is what a promotion or deletion is checked against (ADR-0089).
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
        let list = consumers.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
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
    out.push_str("\n  ],\n  \"stats\": {\n");
    let total_import_edges: usize = by_iface.values().map(|(_, c)| c.len()).sum();
    out.push_str(&format!(
        "    \"total_components\": {},\n    \
             \"total_interfaces\": {},\n    \
             \"total_import_edges\": {},\n    \
             \"unconsumed_exports\": {},\n    \
             \"total_apps\": {}\n  \
         }}\n}}",
        catalog.len(),
        by_iface.len(),
        total_import_edges,
        orphans.len(),
        apps.len()
    ));
    out
}

/// `⟨…⟩` quotes an arbitrary id; only the closing bracket could end it early.
fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

/// Through JSON, so a value cannot carry syntax (ADR-0080).
fn lit(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// Empty when git cannot say: a generation row with no commit still answers "did the counts move".
fn head_commit() -> String {
    let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(holon_fleet::fleet::repo_root())
        .output()
    else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Nodes are upserted; edges get per-generation ids and older ones are deleted, never touching the
/// accumulated `memory`/`task` (ADR-0091). Insert before delete: doubled beats briefly empty.
fn surql(catalog: &Catalog, apps: &[App], generation: u64) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "-- comp-capgraph projection, generation {generation}.\n\
         -- Derived from the built artifacts. Never hand-edit: the next rebuild\n\
         -- overwrites this, and nothing here is anybody's only copy (ADR-0091).\n"
    ));

    // Every interface named by anything, so one nobody provides still gets a node.
    let mut interfaces: BTreeSet<String> = BTreeSet::new();
    for name in catalog.names() {
        if let Some(surface) = catalog.surface(name) {
            interfaces.extend(surface.exports.iter().cloned());
            interfaces.extend(surface.imports.iter().cloned());
        }
    }
    let n_interfaces = interfaces.len();
    for iface in &interfaces {
        let exporter = catalog.exporter(iface).unwrap_or("");
        out.push_str(&format!(
            "UPSERT {} SET name = {}, exporter = {}, consumers = {}, gen = {generation};\n",
            rid("interface", iface),
            lit(iface),
            lit(exporter),
            catalog.consumer_count(iface),
        ));
    }

    // The digest makes a lesson's staleness visible (ADR-0091).
    let mut n_artifacts = 0usize;
    for name in catalog.names() {
        n_artifacts += 1;
        let digest = catalog.bytes(name).map(holon_oci::digest_of).unwrap_or_default();
        out.push_str(&format!(
            "UPSERT {} SET name = {}, digest = {}, gen = {generation};\n",
            rid("artifact", name),
            lit(name),
            lit(&digest),
        ));
    }

    // The closure, not the root: "which interfaces does this app import" covers every part.
    for app in apps {
        out.push_str(&format!(
            "UPSERT {} SET name = {}, root = {}, artifact = {}, gen = {generation};\n",
            rid("app", &app.name),
            lit(&app.name),
            lit(&app.root),
            lit(&app.artifact),
        ));
    }

    let (mut n_imports, mut n_exports, mut n_carries) = (0usize, 0usize, 0usize);
    for name in catalog.names() {
        let Some(surface) = catalog.surface(name) else {
            continue;
        };
        for (table, ifaces) in [("imports", &surface.imports), ("exports", &surface.exports)] {
            for iface in ifaces.iter() {
                if table == "imports" {
                    n_imports += 1
                } else {
                    n_exports += 1
                }
                out.push_str(&format!(
                    "RELATE {}->{}->{} SET gen = {generation};\n",
                    rid("artifact", name),
                    rid(table, &format!("{generation}|{name}|{iface}")),
                    rid("interface", iface),
                ));
            }
        }
    }
    // `closure` excludes the root, whose imports are the ones a lesson is most likely about.
    for app in apps {
        let mut parts = catalog.closure(&app.root);
        if !parts.contains(&app.root) {
            parts.push(app.root.clone());
        }
        for part in parts {
            n_carries += 1;
            out.push_str(&format!(
                "RELATE {}->{}->{} SET gen = {generation};\n",
                rid("app", &app.name),
                rid("carries", &format!("{generation}|{}|{part}", app.name)),
                rid("artifact", &part),
            ));
        }
    }

    // Computed in the database, where the lessons are; this edge is only an index over their tags.
    out.push_str(
        "\n-- Lessons, indexed by the interface they are about (ADR-0090's key, ADR-0091's store).\n",
    );
    // Driven from the lessons: SurrealDB v3.1.3 rejects `RELATE <list>->edge-><one>`. Defined, not
    // written: `SELECT ... FROM memory` errors on a fresh install with no lesson.
    out.push_str("DEFINE TABLE IF NOT EXISTS memory;\n");
    out.push_str("DEFINE TABLE IF NOT EXISTS about TYPE RELATION;\n");
    // An array of pairs first: v3.1.3 rejects a list as `RELATE` source and fails on `NONE`.
    out.push_str(&format!(
        "LET $pairs = (SELECT id AS lesson, \
           (SELECT VALUE id FROM interface WHERE gen = {generation} AND name IN $parent.tags) \
           AS ifaces FROM memory WHERE tags != []);\n\
         FOR $pair IN $pairs {{\n    \
           IF array::len($pair.ifaces) > 0 {{\n        \
             LET $from = $pair.lesson;\n        \
             LET $to = $pair.ifaces;\n        \
             RELATE $from->about->$to SET gen = {generation};\n    \
           }};\n\
         }};\n"
    ));

    // The one row a rebuild adds and never removes, so the graph's movement is recorded. UPSERT
    // keyed by generation keeps re-projection idempotent.
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    out.push_str(&format!(
        "\n-- This build, kept. Never aged out: it is the only history the store has.\n\
         UPSERT {} SET gen = {generation}, at = {at}, commit = {}, \
         interfaces = {n_interfaces}, artifacts = {n_artifacts}, apps = {}, \
         imports = {n_imports}, exports = {n_exports}, carries = {n_carries};\n",
        rid("generation", &generation.to_string()),
        lit(&head_commit()),
        apps.len(),
    ));

    out.push_str("\n-- Age out the previous generation. Seven derived tables, and only\n");
    out.push_str("-- rows older than this run — `memory` and `task` are unreachable from here.\n");
    for table in [
        "interface",
        "artifact",
        "app",
        "imports",
        "exports",
        "carries",
        // The index, never the lessons.
        "about",
    ] {
        out.push_str(&format!("DELETE {table} WHERE gen < {generation};\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0091 rests on this: no statement writes or deletes an accumulated table.
    #[test]
    fn the_projection_never_writes_or_deletes_an_accumulated_table() {
        let root = holon_fleet::fleet::repo_root();
        let catalog = Catalog::scan(&default_dirs(&root));
        if catalog.is_empty() {
            eprintln!("skipped: nothing built — run `cargo xtask build --force`");
            return;
        }
        let out = surql(&catalog, &holon_metadata::app::discover_apps(&root), 42);

        /// `about` is here and `memory` is not: the edge is an index, the lesson what it indexes.
        const DERIVED: &[&str] =
            &["interface", "artifact", "app", "imports", "exports", "carries", "about"];

        /// Keyed `generation:<gen>`, so an UPSERT can only reach this run's row.
        const APPEND_ONLY: &[&str] = &["generation"];

        let mut mutations = 0usize;
        for statement in out.lines().filter(|l| !l.trim_start().starts_with("--")) {
            let mut words = statement.split_whitespace();
            let Some(verb) = words.next() else { continue };
            // Only the verbs that can change a row; `DEFINE`, `LET`, `FOR`, `IF` and `SELECT` cannot.
            if !["UPSERT", "CREATE", "UPDATE", "DELETE", "REMOVE", "RELATE"].contains(&verb) {
                continue;
            }
            let Some(rest) = words.next() else { continue };
            // `RELATE a->edge->b` writes the middle table; other verbs the first one named.
            let target =
                if verb == "RELATE" { rest.split("->").nth(1).unwrap_or_default() } else { rest };
            let table = target.split([':', '⟨']).next().unwrap_or_default().trim_start_matches('$');
            mutations += 1;
            if APPEND_ONLY.contains(&table) {
                assert!(
                    verb != "DELETE" && verb != "REMOVE",
                    "a {verb} targets {table:?} — the rebuild may ADD a generation and may \
                     never remove one, or the store loses the only history it has:\n  {statement}"
                );
                continue;
            }
            assert!(
                DERIVED.contains(&table),
                "a {verb} targets {table:?}, which is not a derived table — a rebuild must \
                 never change a row it cannot recompute:\n  {statement}"
            );
        }
        assert!(mutations > 100, "only {mutations} mutating statements — the parser is wrong");

        // Record ids stripped first: `memory-probe` would trip a substring search.
        for line in out.lines() {
            let t = line.trim();
            if t.starts_with("--") {
                continue;
            }
            let mut outside = String::new();
            let mut depth = 0usize;
            let mut quoted = false;
            for c in t.chars() {
                match c {
                    '"' => quoted = !quoted,
                    _ if quoted => {}
                    '⟨' => depth += 1,
                    '⟩' => depth = depth.saturating_sub(1),
                    _ if depth == 0 => outside.push(c),
                    _ => {}
                }
            }
            if !outside.contains("memory") {
                continue;
            }
            let permitted = outside.starts_with("DEFINE TABLE IF NOT EXISTS memory")
                || (outside.starts_with("LET") && outside.contains("FROM memory"));
            assert!(
                permitted,
                "this statement names the `memory` TABLE in a way nothing has justified — \
                 the projection may define it and read it, and nothing else:\n  {t}"
            );
        }
    }

    /// Or the second rebuild fails on a duplicate id and the projection silently stops updating.
    #[test]
    fn edge_ids_are_generation_scoped_and_nodes_are_not() {
        let root = holon_fleet::fleet::repo_root();
        let catalog = Catalog::scan(&default_dirs(&root));
        if catalog.is_empty() {
            eprintln!("skipped: nothing built — run `cargo xtask build --force`");
            return;
        }
        let one = surql(&catalog, &holon_metadata::app::discover_apps(&root), 1);
        let two = surql(&catalog, &holon_metadata::app::discover_apps(&root), 2);

        let edge_ids = |s: &str| {
            s.lines()
                .filter(|l| l.starts_with("RELATE"))
                .map(|l| l.split("->").nth(1).unwrap_or_default().to_string())
                .collect::<BTreeSet<_>>()
        };
        assert!(!edge_ids(&one).is_empty(), "no edges at all");
        assert!(
            edge_ids(&one).is_disjoint(&edge_ids(&two)),
            "two generations share an edge id — the second RELATE will be refused"
        );

        // `generation` is excluded: its id must move, asserted below.
        let node_ids = |s: &str| {
            s.lines()
                .filter(|l| l.starts_with("UPSERT") && !l.starts_with("UPSERT generation"))
                .map(|l| l.split_whitespace().nth(1).unwrap_or_default().to_string())
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            node_ids(&one),
            node_ids(&two),
            "node ids moved between generations — anything pointing at one would dangle"
        );

        // An unmoved `generation` id would overwrite one row forever while appearing to work.
        let gen_ids = |s: &str| {
            s.lines()
                .filter(|l| l.starts_with("UPSERT generation"))
                .map(|l| l.split_whitespace().nth(1).unwrap_or_default().to_string())
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(gen_ids(&one).len(), 1, "a build must write exactly one generation row");
        assert!(
            gen_ids(&one).is_disjoint(&gen_ids(&two)),
            "two builds share a generation id — the second would overwrite the first"
        );
    }
}
