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
//!   comp-capgraph --format surql  → the same graph as a PROJECTION, for the store
//!
//! The last one is ADR-0091. The graph stops being a report a person reads and
//! becomes rows a query can join against, in the same database the knowledge pool
//! lives in — so "what did previous runs learn about the interfaces this app
//! imports" is one query rather than a tool invocation feeding a second call.
//!
//! This binary stays the SOURCE. What lands in SurrealDB is a projection: written
//! only by a rebuild, never hand-edited, and always safe to drop, because it is
//! recomputed from the built artifacts in under a second. That is what makes the
//! generation stamp below load-bearing rather than decorative.

use std::collections::{BTreeMap, BTreeSet};

use clap::Parser;
use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::{default_dirs, Catalog};

#[derive(Parser)]
#[command(name = "comp-capgraph", about = "The capability graph: who imports what from whom")]
struct Args {
    /// `md` (default), `json`, `mermaid`, or `surql`.
    #[arg(long, default_value = "md")]
    format: String,

    /// The generation to stamp a `surql` projection with (ADR-0091).
    ///
    /// Every derived node and edge carries it, and the projection ends by deleting
    /// everything stamped with anything older. That is the whole of the isolation
    /// between the derived half and the accumulated half now that they share a
    /// database: a rebuild can only ever reach rows it wrote itself, so the one
    /// thing in the system that cannot be recomputed is not reachable from the
    /// thing that is recomputed constantly.
    ///
    /// Defaults to the wall clock in seconds — monotonic enough for "newer than
    /// the last rebuild", which is the only comparison made on it. Pass it
    /// explicitly to get a byte-identical projection out of the same catalogue.
    #[arg(long)]
    gen: Option<u64>,

    /// Ask the catalogue "do we already have something for this?" and print the
    /// answer instead of the graph.
    ///
    /// The question ADR-0089 says a goal should ask before generating an
    /// implementation. No model involved: term overlap over the descriptions and
    /// the exported interface names, with the graph breaking ties towards what
    /// applications already carry.
    #[arg(long)]
    find: Option<String>,

    /// Report every interface exported by more than one component, and stop.
    ///
    /// ADR-0089's duplicate-detection gap, structurally: prevention is
    /// `--find` consulted before writing, and this counts what got through.
    #[arg(long)]
    twins: bool,

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
            // A standard interface exported by everything is not duplication, it
            // is the shape of the system — and printing 65 component names every
            // run buries the lines that are about capabilities somebody built.
            // Still reported, because the count is the interesting part: it is the
            // population that would stop owning its own entry point.
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
        // The silent half. `comp-plug` resolves an interface to ONE exporter with
        // `or_insert_with`, so for every pair above the winner is decided by scan
        // order and nothing says so at compose time.
        println!(
            "\nNote: `comp-plug` picks a single exporter per interface by scan order. For \
             each line above,\nwhichever component sorts first is what every composition \
             silently gets."
        );
        return Ok(());
    }

    if let Some(query) = &args.find {
        let mut apps_of: BTreeMap<String, usize> = BTreeMap::new();
        for app in apps(&root) {
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
                    "(no description in catalog.json)"
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
        "json" => println!("{}", json(&catalog, &by_iface, &orphans, &apps(&root))),
        "surql" => {
            let generation = args.gen.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
            println!("{}", surql(&catalog, &apps(&root), generation));
        }
        "mermaid" => println!("{}", mermaid(&by_iface, args.diagram_top)),
        "md" => {
            println!("{}", markdown(&catalog, &by_iface, &orphans, args.diagram_top, &apps(&root)))
        }
        other => return Err(format!("unknown format {other:?} — md, json, mermaid or surql")),
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

/// A record id. `⟨…⟩` is SurrealDB's own quoting for an arbitrary id, and the
/// closing bracket is the one character that could end the quoting early — so it
/// goes. Interface names carry `:`, `/` and `@`, none of which need escaping
/// inside the brackets, which is why this is three lines rather than a parser.
fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

/// A string literal. Through JSON, so a value cannot carry syntax (ADR-0080).
fn lit(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The capability graph as a projection into the store the knowledge pool lives
/// in (ADR-0091).
///
/// ## Why the shape is what it is
///
/// **Nodes are upserted, edges are generation-scoped.** A node keeps a stable id
/// across rebuilds — `interface:⟨csv:codec/codec@0.1.0⟩` is the same row today and
/// next week — because that id is what anything else in the database points at.
/// An edge cannot: `RELATE` with an id that already exists is an error, so every
/// generation mints its own edge ids and the old ones are deleted at the end.
///
/// **The delete is the isolation.** It names six tables, all six derived, and it
/// is bounded by `gen < N` so it can only ever remove rows an older run of this
/// same function wrote. `memory` and `task` — the accumulated half, the half that
/// cannot be recomputed from anything — are not named here and there is no
/// statement in this output that could reach them. That property is the entire
/// reason ADR-0091 was willing to put both halves in one database, so it is worth
/// checking by reading rather than trusting: every statement below is against
/// `interface`, `artifact`, `app`, `imports`, `exports` or `carries`.
///
/// **Insert before delete.** The new generation lands first and the old one goes
/// afterwards, so a reader between the two sees both rather than neither. A
/// projection that is briefly doubled degrades a ranking; a projection that is
/// briefly empty looks exactly like a codebase nobody has ever learned anything
/// about, which is the wrong answer rather than a slow one.
/// The commit this projection was derived from, or `""` when git cannot say.
///
/// Empty rather than absent on failure: a generation row with no commit still
/// answers "did the counts move", which is most of the value. Refusing to write
/// one because git is unavailable would lose the whole row to a missing label.
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

    // Interfaces. Every name that is exported or imported by anything, so an
    // interface nobody provides still gets a node — that absence is a fact a
    // capability search wants, not a row to omit.
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

    // Artifacts. The digest is what makes a lesson's staleness visible: a lesson
    // is retrieved by interface so that it survives a rebuild, and stamped with
    // the digest it was learned against so that "has this changed underneath the
    // lesson" stays an answerable question rather than an assumption (ADR-0091).
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

    // Apps, and what each one is actually composed from — the closure, not the
    // root, because "which interfaces does this app import" is a question about
    // every part in it.
    for app in apps {
        out.push_str(&format!(
            "UPSERT {} SET name = {}, root = {}, artifact = {}, gen = {generation};\n",
            rid("app", &app.name),
            lit(&app.name),
            lit(&app.root),
            lit(&app.artifact),
        ));
    }

    // Edges.
    let (mut n_imports, mut n_exports, mut n_carries) = (0usize, 0usize, 0usize);
    for name in catalog.names() {
        let Some(surface) = catalog.surface(name) else {
            continue;
        };
        for (table, ifaces) in [("imports", &surface.imports), ("exports", &surface.exports)] {
            for iface in ifaces.iter() {
                if table == "imports" { n_imports += 1 } else { n_exports += 1 }
                out.push_str(&format!(
                    "RELATE {}->{}->{} SET gen = {generation};\n",
                    rid("artifact", name),
                    rid(table, &format!("{generation}|{name}|{iface}")),
                    rid("interface", iface),
                ));
            }
        }
    }
    // `closure` is the root's DEPENDENCIES and does not include the root itself,
    // which is right for "what did this get composed from" and wrong here. The
    // root is the app's own domain component, and its imports are the ones a
    // lesson is most likely to be about — leave it out and `conduit` appears to
    // import three interfaces from its shared parts, when `conduit-domain` alone
    // imports five that nothing else in the query can see.
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

    // `lesson -about-> interface` — the edge ADR-0091 drafted and deferred.
    //
    // Computed in the database rather than here, because this process reads the
    // filesystem and the lessons are rows: the projection emits the join and
    // SurrealDB performs it. That keeps the direction of authority right — a tag
    // on a lesson is the accumulated fact, and this edge is only an INDEX over it,
    // which is what makes it safe to stamp, drop and recompute like every other
    // derived row here.
    //
    // Nothing writes to `memory`. The edge is a separate table; ageing out a
    // generation of `about` rows leaves every lesson exactly as it was, which is
    // the property the whole generation scheme exists to protect.
    out.push_str(
        "\n-- Lessons, indexed by the interface they are about (ADR-0090's key, ADR-0091's store).\n",
    );
    // Driven from the LESSONS, not from the interfaces, and not by preference:
    // SurrealDB v3.1.3 accepts `RELATE <one>->edge-><list>` and rejects the mirror
    // image with "Cannot execute statement using value: interface:`…`". Iterating
    // interfaces and relating a list of lessons to each — the shape with fewer
    // iterations — is the one that does not bind.
    //
    // The cost is a loop over the pool rather than over the 80 interfaces. That is
    // a REBUILD cost, paid by `just capgraph-store` and never on a read, which is
    // the whole reason this is a projection.
    // Defined, not written. `SELECT ... FROM memory` is an error on a database
    // where no lesson has ever been recorded, which is every fresh install — the
    // same class of failure as the namespace that did not exist. A table
    // DEFINITION is not a row: the generation delete below still names `about` and
    // never `memory`, so the accumulated half remains unreachable from here in the
    // only sense that matters.
    out.push_str("DEFINE TABLE IF NOT EXISTS memory;\n");
    out.push_str("DEFINE TABLE IF NOT EXISTS about TYPE RELATION;\n");
    // The pairs are computed into an ARRAY first, and only then iterated.
    //
    // Three shapes were tried against v3.1.3 and two of them are landmines that a
    // seeded test would not have found:
    //
    //   * iterating `interface` and relating a LIST of lessons to each — the shape
    //     with the fewest iterations — is rejected outright: `RELATE` takes a list
    //     as its target but not as its source.
    //   * iterating `memory` directly works only while every lesson matches
    //     something. A pool with no lessons fails on `NONE`, and a lesson whose
    //     tags name no interface fails on an empty target — so the version that
    //     passed a happy-path test would have broken on the first fresh install
    //     and on the first lesson about a retired interface.
    //
    // Hence: build `{lesson, interfaces}` rows, skip the empty ones, relate the
    // rest. `$parent` is how the inner SELECT reaches the row it belongs to.
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

    // What this build WAS — the one row a rebuild adds and can never take away.
    //
    // Everything else here is a projection of *now*: rewritten whole, stamped, and
    // aged out below, so the store answers "what is the graph" and cannot answer
    // "what did it used to be". That was fine while the graph was a report someone
    // read. It stops being fine the moment anyone asks whether the graph MOVED —
    // and there is no cheaper place to answer that than at the point the counts are
    // already in hand.
    //
    // A third category, and worth naming as one. `memory` is accumulated and the
    // rebuild may not touch it. The seven derived tables are recomputable and the
    // rebuild owns them. This is accumulated — a past build's counts cannot be
    // recovered once lost — and yet the rebuild is the only thing that can write it.
    // What keeps that safe is the id: keyed by generation, so an UPSERT can reach
    // exactly the row this run is writing and no other. Same argument the delete
    // below rests on, pointed the other way.
    //
    // UPSERT and not CREATE so that re-projecting one generation is idempotent
    // rather than an error — `just capgraph-store` twice in the same second is a
    // re-run of one build, not two builds.
    //
    // One row per build, ~10 fields. Unbounded in principle; at a projection per CI
    // run it is the cheapest table in the database by three orders of magnitude,
    // and it is the only one that would notice if the projection stopped running.
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
        // The index, never the lessons. `about` rows are recomputed from
        // `memory.tags` on every projection; deleting them cannot lose a lesson.
        "about",
    ] {
        out.push_str(&format!("DELETE {table} WHERE gen < {generation};\n"));
    }
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
#[cfg(test)]
mod tests {
    use super::*;

    /// The projection must not be able to touch the accumulated half.
    ///
    /// This is the check ADR-0091 rests on. Both halves share one database, and the
    /// only thing between a rebuild and the one table that cannot be recomputed is
    /// this function.
    ///
    /// It used to assert that the projection never NAMED `memory`. That was the
    /// right instinct and the wrong rule, and ADR-0093 broke it for a good reason:
    /// indexing lessons by interface means reading `memory` and defining it, since
    /// `SELECT` on a table that does not exist is an error on every fresh install.
    /// Reading cannot lose a lesson. Neither can a table definition.
    ///
    /// So the rule is now what it always meant: **no statement WRITES OR DELETES an
    /// accumulated table.** That is strictly stronger than the old one, because the
    /// old one only inspected the second whitespace token and would have waved
    /// through any statement shape it had not anticipated — which is exactly what
    /// happened when the first `LET`/`FOR` block was added.
    #[test]
    fn the_projection_never_writes_or_deletes_an_accumulated_table() {
        let root = comp_reconciler::fleet::repo_root();
        let catalog = Catalog::scan(&default_dirs(&root));
        if catalog.is_empty() {
            eprintln!("skipped: nothing built — run `just build`");
            return;
        }
        let out = surql(&catalog, &apps(&root), 42);

        /// Tables a rebuild is allowed to create rows in and delete rows from.
        /// `about` is here and `memory` is not: the edge is an index over a tag,
        /// recomputed every build, and the lesson is the thing it indexes.
        const DERIVED: &[&str] =
            &["interface", "artifact", "app", "imports", "exports", "carries", "about"];

        /// Written by the rebuild, never recomputable, and never deleted by it.
        /// The safety is in the key, not in the verb: `generation:<gen>` means an
        /// UPSERT can only reach the row this run is writing, so a rebuild cannot
        /// rewrite a build that already happened. `DELETE` is checked separately
        /// below, because that is the verb that could lose one.
        const APPEND_ONLY: &[&str] = &["generation"];

        let mut mutations = 0usize;
        for statement in out.lines().filter(|l| !l.trim_start().starts_with("--")) {
            let mut words = statement.split_whitespace();
            let Some(verb) = words.next() else { continue };
            // Only the verbs that can change a row. `DEFINE`, `LET`, `FOR`, `IF`
            // and `SELECT` cannot, whatever they name.
            if !["UPSERT", "CREATE", "UPDATE", "DELETE", "REMOVE", "RELATE"].contains(&verb) {
                continue;
            }
            let Some(rest) = words.next() else { continue };
            // `RELATE a->edge->b` writes to the EDGE table in the middle; every
            // other verb writes to the table it names first.
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

        // And the reads, separately: naming `memory` is legal now, so state exactly
        // what may be done to it rather than leaving it open.
        //
        // Record IDS are stripped first. `artifact:⟨memory-probe⟩` and
        // `artifact:⟨knowledge-memory⟩` are components in this repository whose
        // NAMES contain the word, and a substring search over the raw line flags
        // both — which is the mistake the previous version of this test warned
        // about in a comment and this version made anyway.
        for line in out.lines() {
            let t = line.trim();
            if t.starts_with("--") {
                continue;
            }
            // Everything that is DATA rather than syntax comes out: record ids in
            // `⟨…⟩` and string literals in `"…"`. `interface:⟨knowledge:memory/…⟩`
            // and `SET exporter = "knowledge-memory"` both contain the word and
            // neither is a reference to the table.
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

    /// Every generation mints its own edge ids, or the second rebuild fails on a
    /// duplicate id and the projection silently stops updating.
    #[test]
    fn edge_ids_are_generation_scoped_and_nodes_are_not() {
        let root = comp_reconciler::fleet::repo_root();
        let catalog = Catalog::scan(&default_dirs(&root));
        if catalog.is_empty() {
            eprintln!("skipped: nothing built — run `just build`");
            return;
        }
        let one = surql(&catalog, &apps(&root), 1);
        let two = surql(&catalog, &apps(&root), 2);

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

        // `generation` is excluded on purpose, and asserted separately below: its id
        // MUST move, because one row per build is the entire point of the table.
        // Nothing points at it, so nothing can dangle.
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

        // The mirror image, and the reason the exclusion above is safe rather than
        // convenient: two builds must land in two rows. A `generation` id that did
        // NOT move would mean each build silently overwrote the last one, and the
        // table would hold one row forever while appearing to work.
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
