//! The capability graph as a projection, against a real database (ADR-0091).
//!
//! The unit tests in `capgraph.rs` check the SurrealQL the tool *writes*; this checks what the
//! database *answers*, where two load-bearing claims live:
//!
//!   1. **The join works.** An app's lessons are reachable from the app through its parts and the
//!      interfaces they import, with nothing in the query naming the app's subject matter.
//!   2. **The rebuild cannot reach the accumulated half.** Only a generation stamp separates a
//!      rebuild from the one table that cannot be recomputed.
//!
//! No model is involved, so nothing is mocked. Driven through the real binary (`--format surql`
//! piped into `/sql`, as the projection is stored). Skipped, loudly, when Docker cannot start the
//! database.

use std::process::Command;

// The shared test module of the root's suites, included rather than copied (docs/adr/0115).
#[path = "../../tests/harness/mod.rs"]
mod harness;
use harness::{Store, SURREAL_IMAGE};

/// The app the join is asserted against. It reaches `csv:codec/codec` through a part rather than
/// its own root, so a pass cannot be explained by the app node alone.
const APP: &str = "vet";

/// Write one generation of the projection, exactly as storing it by hand does: the tool's stdout,
/// unedited, posted to `/sql`. Not on `harness::Store` because it is capgraph-specific.
fn project(db: &Store, generation: u64) {
    let out = Command::new(env!("CARGO_BIN_EXE_comp-capgraph"))
        .args(["--format", "surql", "--gen", &generation.to_string()])
        .output()
        .expect("comp-capgraph did not run");
    assert!(out.status.success(), "comp-capgraph failed: {}", String::from_utf8_lossy(&out.stderr));
    let sql = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(sql.contains("UPSERT interface:"), "projection wrote no interfaces");
    db.last(&sql);
}

/// The projection is aimed at the database the lessons are in.
///
/// Other tests start their own container and write both halves of the join into it, so all passed
/// while production had the capability graph in `holon`/`holon` and the pool in `comp`/`goalmemory`.
/// So this asserts the COORDINATES by reading the files that declare them; no database needed.
#[test]
fn the_projection_targets_the_database_the_pool_lives_in() {
    let root = comp_fleet::fleet::root_from_manifest();
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();

    // What the component falls back to when `surreal-ns` is unset, and what the driver rewrites the
    // memory app's database to.
    assert!(
        read("components/knowledge-graph/src/lib.rs").contains(r#"cfg("surreal-ns", "comp")"#),
        "knowledge-graph's default namespace moved — the Justfile recipes need to move with it"
    );
    // `goalrun.rs` AND its submodules: the coordinate moved into `goalrun/setup.rs`, and reading one
    // file would go red for a refactor that changed no coordinate.
    let goalrun = {
        let mut all = read("reconciler/goal/src/bin/goalrun.rs");
        if let Ok(dir) = std::fs::read_dir(root.join("reconciler/goal/src/bin/goalrun")) {
            for e in dir.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "rs")) {
                all.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
            }
        }
        all
    };
    assert!(
        goalrun.contains(r#"("SURREAL_DB", "goalmemory")"#),
        "comp-goalrun no longer points the pool at `goalmemory` — the projection is aimed elsewhere"
    );

    // Both recipes, because `lessons-for` reading a database `capgraph-store` never wrote is the same
    // bug. The root file AND everything it imports (recipes live in `just/host.just`).
    let justfile = {
        let Ok(root) = std::fs::read_to_string(root.join("Justfile")) else {
            return;
        };
        let mut all = root.clone();
        for line in root.lines() {
            if let Some(rest) = line.trim().strip_prefix("import ") {
                all.push('\n');
                all.push_str(&read(rest.trim().trim_matches(|c| c == '\'' || c == '"')));
            }
        }
        all
    };

    // By RECIPE, not by counting lines (counting `SURREAL_NS:-` went red when `host-console` gained a
    // namespace): the two recipes that READ AND WRITE the pool have to agree on where it is.
    let recipe = |name: &str| -> String {
        let at = justfile
            .find(&format!("\n{name}"))
            .unwrap_or_else(|| panic!("the `{name}` recipe is gone from the Justfile"));
        justfile[at + 1..]
            .lines()
            .skip(1)
            .take_while(|l| l.trim().is_empty() || l.starts_with([' ', '\t', '#']))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for name in ["capgraph-store", "lessons-for"] {
        let body = recipe(name);
        assert!(
            body.contains("SURREAL_NS:-comp"),
            "`{name}` defaults to a namespace the knowledge graph is not in"
        );
        assert!(
            body.contains("SURREAL_DB:-goalmemory"),
            "`{name}` defaults to a database the knowledge pool is not in"
        );
    }

    // And nothing anywhere may default to somewhere else.
    for line in justfile.lines() {
        if line.contains("SURREAL_NS:-") {
            assert!(line.contains("SURREAL_NS:-comp"), "wrong namespace default: {}", line.trim());
        }
        if line.contains("SURREAL_DB:-") {
            assert!(
                line.contains("SURREAL_DB:-goalmemory"),
                "wrong database default: {}",
                line.trim()
            );
        }
    }
}

/// The connections survive being written to the database: every edge lands on a node that exists,
/// every app carries its own root, and the tool's counts match the edges the database holds.
#[test]
fn the_projection_preserves_the_app_and_component_connections() {
    let Some(db) = Store::start() else {
        eprintln!(
            "SKIPPED: docker could not start {SURREAL_IMAGE} — the connections are unverified"
        );
        return;
    };
    project(&db, 1);

    // 1. No edge points at a node that is not there (`out.name = NONE` is a dangling record link).
    for edge in ["carries", "imports", "exports"] {
        let dangling =
            db.last(&format!("SELECT count() FROM {edge} WHERE out.name = NONE GROUP ALL;"));
        assert_eq!(
            dangling[0]["count"].as_u64().unwrap_or(0),
            0,
            "{edge} has edges pointing at nodes that do not exist"
        );
    }

    // 2. Every app carries its own root (the one place the projection differs from `--format json`).
    let rootless =
        db.last("SELECT VALUE name FROM app WHERE root NOT IN ->carries->artifact.name;");
    assert_eq!(
        rootless.as_array().map(|a| a.len()),
        Some(0),
        "these apps do not carry their own root, so their domain component's \
         imports are invisible to the join: {rootless:?}"
    );

    // 3. The consumer count on each interface agrees with the import edges held; computed by
    //    different code paths, so a disagreement means one is wrong.
    let mismatched =
        db.last("SELECT VALUE name FROM interface WHERE consumers != array::len(<-imports);");
    assert_eq!(
        mismatched.as_array().map(|a| a.len()),
        Some(0),
        "consumer counts disagree with the import edges for: {mismatched:?}"
    );

    // 4. Named compositions, so a wholesale collapse cannot pass by being uniformly empty.
    for (app, part) in
        [("conduit", "auth-guard"), ("saga", "fsm-workflow"), ("helpdesk", "record-store")]
    {
        let carried = db.last(&format!(
            "SELECT VALUE array::sort(->carries->artifact.name) FROM ONLY app:⟨{app}⟩;"
        ));
        let has =
            carried.as_array().map(|a| a.iter().any(|x| x.as_str() == Some(part))).unwrap_or(false);
        assert!(has, "{app} does not carry {part} in the store: {carried:?}");
    }

    // 5. Every artifact has a digest, or Q9's staleness stamp has nothing to stamp.
    let undigested = db.last("SELECT count() FROM artifact WHERE digest = '' GROUP ALL;");
    assert_eq!(
        undigested[0]["count"].as_u64().unwrap_or(0),
        0,
        "artifacts landed without a digest — a lesson cannot record what it was \
         learned against"
    );
}

/// The lessons the projection must never be able to touch. Tagged with an interface `vet` imports
/// and one nothing does, so the join is asserted to be selective rather than merely non-empty.
const SEED: &str = r#"
UPSERT memory:⟨probe-csv⟩ SET ns = 'errors',
  text = 'Dialect.delimiter is a String, not a char',
  tags = ['csv:codec/codec@0.1.0'], uses = 0;
UPSERT memory:⟨probe-nowhere⟩ SET ns = 'errors',
  text = 'a lesson about an interface nothing in this repository imports',
  tags = ['nobody:at-all/nothing@9.9.9'], uses = 0;
"#;

#[test]
fn the_join_works_and_a_rebuild_cannot_reach_the_lessons() {
    let Some(db) = Store::start() else {
        eprintln!("SKIPPED: docker could not start {SURREAL_IMAGE} — the join is unverified");
        return;
    };

    db.last(SEED);
    assert_eq!(db.count("memory"), 2, "the fixture did not seed the accumulated half");

    project(&db, 1);

    // The derived half arrived. Exact counts are not asserted: they move with every new component.
    for table in ["interface", "artifact", "app", "imports", "exports", "carries"] {
        assert!(db.count(table) > 0, "the projection wrote no {table} rows");
    }

    let query = format!(
        "LET $ifaces = (SELECT VALUE array::distinct(array::flatten(\
           ->carries->artifact->imports->interface.name)) FROM ONLY app:⟨{APP}⟩);\n\
         SELECT text FROM memory WHERE tags CONTAINSANY $ifaces;"
    );
    let hits = db.last(&query);
    let texts: Vec<&str> = hits
        .as_array()
        .map(|a| a.iter().filter_map(|h| h["text"].as_str()).collect())
        .unwrap_or_default();

    assert!(
        texts.iter().any(|t| t.contains("Dialect.delimiter")),
        "the join found nothing about an interface {APP} imports — either the \
         capability graph or the edge into the knowledge pool is broken. got: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("nothing in this repository imports")),
        "the join returned a lesson tagged with an interface no app imports — it is \
         matching everything, which would pass this test for the wrong reason"
    );

    let before = db.count("memory");
    project(&db, 2);
    assert_eq!(
        db.count("memory"),
        before,
        "a rebuild changed the accumulated half — the generation stamp is not \
         holding, and the only unrecoverable data in the system is reachable from \
         the half that gets recomputed constantly"
    );

    // And the old generation is gone rather than doubled, or the projection grows without bound.
    let gens = db.last("SELECT VALUE array::distinct(gen) FROM interface GROUP ALL;");
    assert_eq!(
        gens[0].as_array().map(|a| a.len()),
        Some(1),
        "more than one generation of derived rows survived: {gens:?}"
    );

    // The join still answers after a rebuild: node ids are stable across generations.
    let after = db.last(&query);
    assert_eq!(after, hits, "the join gave a different answer after a rebuild");
}

/// The lesson is reached by TRAVERSING an edge, not by scanning the pool.
///
/// ADR-0091 left retrieval as `memory WHERE tags CONTAINSANY`, a full scan of the half that does not
/// scale. The claim: the traversal finds exactly what the scan finds (less means a lossy index,
/// more means matching what the tags do not say). And `about` is a DERIVED row aged out by
/// generation, so a rebuild must not reach a lesson while dropping its index.
#[test]
fn a_lesson_is_reached_through_the_interface_it_is_about() {
    let Some(db) = Store::start() else {
        eprintln!("SKIPPED: docker could not start {SURREAL_IMAGE} — the edge is unverified");
        return;
    };

    db.last(SEED);
    let lessons_before = db.count("memory");
    assert_eq!(lessons_before, 2, "the fixture did not seed the accumulated half");

    project(&db, 1);
    assert!(db.count("about") > 0, "the projection wrote no `about` edges");

    // What the SCAN finds: the old path, kept as the reference answer.
    let scanned = db.last(&format!(
        "LET $ifaces = (SELECT VALUE array::distinct(array::flatten(\
           ->carries->artifact->imports->interface.name)) FROM ONLY app:⟨{APP}⟩);\n\
         SELECT VALUE text FROM memory WHERE tags CONTAINSANY $ifaces ORDER BY text;"
    ));

    // What the TRAVERSAL finds. Nothing in it mentions a tag.
    let traversed = db.last(&format!(
        "LET $ls = (SELECT VALUE array::distinct(array::flatten(\
           ->carries->artifact->imports->interface<-about<-memory)) FROM ONLY app:⟨{APP}⟩);\n\
         SELECT VALUE text FROM $ls ORDER BY text;"
    ));

    assert_eq!(
        traversed, scanned,
        "the edge and the scan disagree about what {APP} should read. The edge is an INDEX \
         over `memory.tags` and nothing else, so any difference means the projection built \
         it wrong — not that one of them is a better answer."
    );
    assert!(
        traversed.as_array().is_some_and(|a| !a.is_empty()),
        "both paths returned nothing, so they agree about nothing — the fixture no longer \
         seeds a lesson tagged with an interface {APP} imports"
    );

    // Rebuilding drops every edge of the old generation; the lessons are untouched (the index is
    // recomputable, they are not).
    project(&db, 2);
    assert_eq!(
        db.count("memory"),
        lessons_before,
        "a rebuild changed the number of lessons — the projection reached the one half of \
         this database that cannot be recomputed from anything"
    );
    let after = db.last(&format!(
        "LET $ls = (SELECT VALUE array::distinct(array::flatten(\
           ->carries->artifact->imports->interface<-about<-memory)) FROM ONLY app:⟨{APP}⟩);\n\
         SELECT VALUE text FROM $ls ORDER BY text;"
    ));
    assert_eq!(after, traversed, "the rebuilt index does not answer what the first one did");
    let stale = db.last("SELECT count() FROM about WHERE gen < 2 GROUP ALL;");
    assert_eq!(
        stale[0]["count"].as_u64().unwrap_or(0),
        0,
        "edges from the previous generation survived the rebuild"
    );
}

/// The index survives a pool with nothing in it, and a lesson about nothing. Both broke early
/// versions of the `about` projection, which runs on every build and must not depend on agreeable
/// data: a fresh install (no lesson ever written) failed on `NONE`, and a lesson tagged with a
/// retired interface (normal under append-only interfaces, ADR-0091) failed on an empty relate target.
#[test]
fn the_index_survives_an_empty_pool_and_a_lesson_about_nothing() {
    let Some(db) = Store::start() else {
        eprintln!(
            "SKIPPED: docker could not start {SURREAL_IMAGE} — the edge cases are unverified"
        );
        return;
    };

    project(&db, 1);
    assert_eq!(db.count("about"), 0, "an empty pool produced edges out of nowhere");

    // 2. A lesson about an interface this repository does not have: normal after a retirement.
    db.last(
        "CREATE memory SET ns = 'patterns', text = 'about something retired', \
         tags = ['gone:forever/interface@0.1.0'];",
    );
    project(&db, 2);
    assert_eq!(
        db.count("memory"),
        1,
        "the projection removed a lesson whose interface no longer exists — those are \
         exactly the lessons that cannot be recomputed from anything"
    );
    assert_eq!(db.count("about"), 0, "a lesson about no known interface was given an edge");

    // 3. A real one still lands, so the above are not passing because the projection stopped writing edges.
    db.last(SEED);
    project(&db, 3);
    assert!(
        db.count("about") > 0,
        "no edges after seeding real lessons — the projection writes nothing, and the \
         two assertions above prove nothing"
    );
}

/// A rebuild adds a generation and takes none away.
///
/// The derived tables are rewritten whole and aged out; `generation` is the one row a projection
/// leaves behind, and the claim is that a `DELETE` running against seven other tables misses it.
/// Asserted against a real database, since the emitted SQL already looks right.
#[test]
fn a_rebuild_adds_a_generation_and_removes_none() {
    let Some(db) = Store::start() else {
        eprintln!("SKIPPED: docker could not start {SURREAL_IMAGE} — generations unverified");
        return;
    };

    project(&db, 1);
    let derived_after_one = db.count("artifact");
    assert!(derived_after_one > 0, "the projection wrote no artifacts");
    assert_eq!(db.count("generation"), 1, "the first projection recorded no generation");

    project(&db, 2);
    project(&db, 3);

    assert_eq!(
        db.count("generation"),
        3,
        "three builds did not leave three rows — either the id is not generation-scoped \
         (each build overwriting the last) or the age-out is reaching this table"
    );

    // The DERIVED tables must NOT have grown, or the age-out stopped working and unfiltered queries
    // return three graphs stacked (more rows, not an error).
    assert_eq!(
        db.count("artifact"),
        derived_after_one,
        "the derived half grew across generations — the age-out is not running, and \
         every query that does not filter on `gen` is now silently wrong"
    );

    // The oldest row still says what it said: an UPSERT keyed by generation only reaches its own row.
    let first = db.last("SELECT gen, artifacts FROM generation WHERE gen = 1;");
    let row = first.as_array().and_then(|a| a.first().cloned()).unwrap_or_default();
    assert_eq!(
        row["artifacts"].as_u64(),
        Some(derived_after_one),
        "generation 1's counts changed after later builds — the row is not immutable"
    );
}
