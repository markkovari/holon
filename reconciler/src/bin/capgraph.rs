//! `comp-capgraph` — what is using what, across every built component.
//!
//! Not "what does this app depend on" (a build file says that) but the reverse, which nobody can
//! answer from memory at 150 components and 93 exported interfaces:
//!
//! * **May I change this interface?** `records:store/store` has 37 consumers, `slug:generate/generator`
//!   has one.
//! * **Does this capability already exist?** The first question a goal should ask before generating
//!   an implementation (ADR-0089).
//! * **What breaks if this component goes?** Its export list, crossed with who imports those.
//!
//! Derived from the BUILT artifacts every time, never maintained by hand: a component's real imports
//! are in the binary, and a second copy can only disagree.
//!
//!   comp-capgraph                 → the markdown report (docs/CAPABILITY-GRAPH.md)
//!   comp-capgraph --format json   → the same graph, for tooling
//!   comp-capgraph --format mermaid → just the diagram
//!   comp-capgraph --format surql  → the same graph as a PROJECTION, for the store
//!
//! `--format surql` is ADR-0091: the graph becomes rows a query can join against, in the same database
//! as the knowledge pool.
//!
//! This binary stays the SOURCE. What lands in SurrealDB is a projection: written only by a rebuild,
//! never hand-edited, always safe to drop (recomputed in under a second), which is what makes the
//! generation stamp below load-bearing.

use std::collections::{BTreeMap, BTreeSet};

use clap::Parser;
use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::{default_dirs, Catalog};

#[derive(Parser)]
#[command(name = "comp-capgraph", about = "The capability graph: who imports what from whom")]
struct Args {
    /// `json` or `surql` (default).
    #[arg(long, default_value = "surql")]
    format: String,

    /// The generation to stamp a `surql` projection with (ADR-0091).
    /// Every derived node and edge carries it, and the projection ends by deleting everything stamped
    /// older. That is the isolation between the derived and accumulated halves sharing a database: a
    /// rebuild only ever reaches rows it wrote itself.
    /// Defaults to the wall clock in seconds (only "newer than the last rebuild" is compared). Pass it
    /// explicitly for a byte-identical projection of the same catalogue.
    #[arg(long)]
    gen: Option<u64>,

    /// Ask the catalogue "do we already have something for this?" and print the answer instead of the
    /// graph.
    /// No model: term overlap over descriptions and exported interface names, graph breaking ties towards
    /// what applications already carry (ADR-0089).
    #[arg(long)]
    find: Option<String>,

    /// Report every interface exported by more than one component, and stop.
    /// ADR-0089's duplicate-detection gap: prevention is `--find` before writing; this counts what got
    /// through.
    #[arg(long)]
    twins: bool,
}

use comp_metadata::app::App;

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
        let found = comp_reconciler::capsearch::twins(&catalog);
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
            // A standard interface exported by everything is not duplication but the shape of the system, and
            // printing 65 names buries the real lines. Still reported: the count is the interesting part.
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
        // The silent half: `comp-plug` resolves an interface to ONE exporter with `or_insert_with`, so for
        // every pair above the winner is decided by scan order and nothing says so at compose time.
        println!(
            "\nNote: `comp-plug` picks a single exporter per interface by scan order. For \
             each line above,\nwhichever component sorts first is what every composition \
             silently gets."
        );
        return Ok(());
    }

    if let Some(query) = &args.find {
        let mut apps_of: BTreeMap<String, usize> = BTreeMap::new();
        for app in comp_metadata::app::discover_apps(&root) {
            for part in catalog.closure(&app.root) {
                *apps_of.entry(part).or_default() += 1;
            }
        }
        let pool = comp_reconciler::capsearch::capabilities(&root, &catalog, &apps_of);
        let hits = comp_reconciler::capsearch::find(query, &pool);
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
            json(&catalog, &by_iface, &orphans, &comp_metadata::app::discover_apps(&root))
        ),
        "surql" => {
            let generation = args.gen.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
            println!("{}", surql(&catalog, &comp_metadata::app::discover_apps(&root), generation));
        }
        other => return Err(format!("unknown format {other:?} — json or surql")),
    }
    Ok(())
}

/// The same graph, as data rather than a table: ADR-0089's capability search needs it, and "how many
/// apps carry this" is the number a promotion or deletion should be checked against.
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

/// A record id. `⟨…⟩` is SurrealDB's quoting for an arbitrary id; the closing bracket is the one
/// character that could end it early, so it goes. Interface names carry `:`, `/` and `@`, which need
/// no escaping inside.
fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

/// A string literal. Through JSON, so a value cannot carry syntax (ADR-0080).
fn lit(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The capability graph as a projection into the store the knowledge pool lives in (ADR-0091).
/// ## Why the shape is what it is
/// **Nodes are upserted, edges are generation-scoped.** A node keeps a stable id across rebuilds
/// because anything else in the database points at it. `RELATE` with an existing id is an error, so
/// every generation mints its own edge ids and the old ones are deleted at the end.
/// **The delete is the isolation.** It names six derived tables and is bounded by `gen < N`, so it can
/// only remove rows an older run of this function wrote. `memory` and `task` (the accumulated half,
/// not recomputable) are not named in any statement. ADR-0091 relies on this to share one database:
/// every statement below is against `interface`, `artifact`, `app`, `imports`, `exports` or `carries`.
/// **Insert before delete.** A reader between the two sees both rather than neither: a briefly doubled
/// projection degrades a ranking, a briefly empty one looks like a codebase nobody learned anything
/// about.
/// The commit this projection was derived from, or `""` when git cannot say.
/// Empty rather than absent on failure: a generation row with no commit still answers "did the counts
/// move".
fn head_commit() -> String {
    let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(comp_reconciler::fleet::repo_root())
        .output()
    else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn surql(catalog: &Catalog, apps: &[App], generation: u64) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "-- comp-capgraph projection, generation {generation}.\n\
         -- Derived from the built artifacts. Never hand-edit: the next rebuild\n\
         -- overwrites this, and nothing here is anybody's only copy (ADR-0091).\n"
    ));

    // Interfaces: every name exported or imported by anything, so an interface nobody provides still gets
    // a node; that absence is a fact a capability search wants.
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

    // Artifacts. The digest makes a lesson's staleness visible: a lesson is retrieved by interface so it
    // survives a rebuild, and stamped with the digest it was learned against (ADR-0091).
    let mut n_artifacts = 0usize;
    for name in catalog.names() {
        n_artifacts += 1;
        let digest = catalog.bytes(name).map(comp_reconciler::oci::digest_of).unwrap_or_default();
        out.push_str(&format!(
            "UPSERT {} SET name = {}, digest = {}, gen = {generation};\n",
            rid("artifact", name),
            lit(name),
            lit(&digest),
        ));
    }

    // Apps, and what each is composed from: the closure, not the root, since "which interfaces does this
    // app import" is about every part in it.
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
    // `closure` is the root's DEPENDENCIES and excludes the root itself, which is wrong here: the root is
    // the app's own domain component and its imports are the ones a lesson is most likely about
    // (`conduit-domain` alone imports five that nothing else in the query can see).
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

    // `lesson -about-> interface`, computed in the database because this process reads the filesystem and
    // the lessons are rows. A tag on a lesson is the accumulated fact; this edge is only an INDEX over
    // it, safe to stamp, drop and recompute. Nothing writes to `memory`.
    out.push_str(
        "\n-- Lessons, indexed by the interface they are about (ADR-0090's key, ADR-0091's store).\n",
    );
    // Driven from the LESSONS, not the interfaces: SurrealDB v3.1.3 accepts `RELATE <one>->edge-><list>`
    // and rejects the mirror image ("Cannot execute statement using value: interface:`…`"). The cost
    // is a loop over the pool, paid only on a REBUILD (`--format surql` posted to `/sql`), never on a
    // read.
    // Defined, not written: `SELECT ... FROM memory` is an error on a database where no lesson was ever
    // recorded (every fresh install). A table DEFINITION is not a row; the generation delete still
    // never names `memory`.
    out.push_str("DEFINE TABLE IF NOT EXISTS memory;\n");
    out.push_str("DEFINE TABLE IF NOT EXISTS about TYPE RELATION;\n");
    // The pairs are computed into an ARRAY first, and only then iterated. Against v3.1.3: iterating
    // `interface` and relating a LIST of lessons is rejected (`RELATE` takes a list as target, not
    // source); iterating `memory` directly fails on `NONE` for a pool with no lessons and on an empty
    // target for a lesson whose tags name no interface, which a happy-path test would not find. So
    // build `{lesson, interfaces}` rows, skip the empty ones, relate the rest. `$parent` is how the
    // inner SELECT reaches its row.
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

    // What this build WAS: the one row a rebuild adds and can never take away. Everything else is a
    // projection of *now* and cannot answer whether the graph MOVED. It is a third category:
    // accumulated (a past build's counts cannot be recovered) yet only the rebuild writes it; the id,
    // keyed by generation, lets an UPSERT reach exactly the row this run is writing. UPSERT and not
    // CREATE so re-projecting one generation is idempotent. One row per build, ~10 fields: the cheapest
    // table, and the only one that would notice if the projection stopped running.
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
        // The index, never the lessons: `about` rows are recomputed from `memory.tags` on every projection.
        "about",
    ] {
        out.push_str(&format!("DELETE {table} WHERE gen < {generation};\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The projection must not be able to touch the accumulated half.
    /// ADR-0091 rests on this check: both halves share one database and only this function stands between
    /// a rebuild and the one table that cannot be recomputed.
    /// The rule is: **no statement WRITES OR DELETES an accumulated table.** Naming `memory` is legal
    /// (ADR-0093: indexing lessons means reading and defining it); reading cannot lose a lesson. An
    /// older rule inspected only the second whitespace token and would wave through any statement shape
    /// it had not anticipated.
    #[test]
    fn the_projection_never_writes_or_deletes_an_accumulated_table() {
        let root = comp_reconciler::fleet::repo_root();
        let catalog = Catalog::scan(&default_dirs(&root));
        if catalog.is_empty() {
            eprintln!("skipped: nothing built — run `cargo xtask build --force`");
            return;
        }
        let out = surql(&catalog, &comp_metadata::app::discover_apps(&root), 42);

        /// Tables a rebuild is allowed to create rows in and delete rows from. `about` is here and `memory`
        /// is not: the edge is an index over a tag, the lesson is the thing it indexes.
        const DERIVED: &[&str] =
            &["interface", "artifact", "app", "imports", "exports", "carries", "about"];

        /// Written by the rebuild, never recomputable, never deleted by it. The safety is in the key:
        /// `generation:<gen>` means an UPSERT can only reach this run's row. `DELETE` is checked separately.
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
            // `RELATE a->edge->b` writes to the EDGE table in the middle; other verbs write to the table they
            // name first.
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

        // And the reads, separately: state exactly what may be done to `memory`. Record IDS are stripped
        // first, since components named `memory-probe` and `knowledge-memory` would trip a substring search.
        for line in out.lines() {
            let t = line.trim();
            if t.starts_with("--") {
                continue;
            }
            // Everything that is DATA rather than syntax comes out: record ids in `⟨…⟩` and string literals.
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

    /// Every generation mints its own edge ids, or the second rebuild fails on a duplicate id and the
    /// projection silently stops updating.
    #[test]
    fn edge_ids_are_generation_scoped_and_nodes_are_not() {
        let root = comp_reconciler::fleet::repo_root();
        let catalog = Catalog::scan(&default_dirs(&root));
        if catalog.is_empty() {
            eprintln!("skipped: nothing built — run `cargo xtask build --force`");
            return;
        }
        let one = surql(&catalog, &comp_metadata::app::discover_apps(&root), 1);
        let two = surql(&catalog, &comp_metadata::app::discover_apps(&root), 2);

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

        // `generation` is excluded on purpose and asserted separately: its id MUST move, because one row per
        // build is the point of the table.
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

        // The mirror image: a `generation` id that did NOT move would mean each build overwrites the last,
        // and the table would hold one row forever while appearing to work.
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
