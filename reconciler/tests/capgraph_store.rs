//! The capability graph as a projection, against a real database (ADR-0091).
//!
//! The unit tests in `capgraph.rs` check the SurrealQL the tool *writes*. This
//! checks what the database *answers*, which is the half this repository has got
//! wrong before (ADR-0061, ADR-0080) and the half ADR-0091's two load-bearing
//! claims actually live in:
//!
//!   1. **The join works.** An app's lessons can be reached from the app, through
//!      the parts it carries and the interfaces those parts import — with nothing
//!      in the query mentioning the app's subject matter. This is what was not
//!      possible while the two graphs were separate stores, and it is the reason
//!      the stores merged.
//!   2. **The rebuild cannot reach the accumulated half.** Both halves now share
//!      one database and the only thing between a rebuild and the one table that
//!      cannot be recomputed is a generation stamp. That is a claim about
//!      behaviour, so it is tested by rebuilding on top of real lessons rather
//!      than by reading the emitted statements.
//!
//! No model is involved anywhere in here, so there is nothing to mock: the query
//! is SurrealQL over interface names, which are identifiers. The embedding side of
//! `knowledge:memory` is a different path with its own tests.
//!
//! Driven through the real binary rather than by calling the generator directly —
//! `--format surql` piped into `/sql` is exactly what `just capgraph-store` does,
//! so a break in the wiring fails here too.
//!
//! Skipped, loudly, when Docker cannot start the database. A skipped test that
//! says so is honest; one that passes because it did nothing is not.

use std::process::Command;

mod harness;
use harness::{Store, SURREAL_IMAGE};

/// The app the join is asserted against. It reaches `csv:codec/codec` through a
/// part rather than through its own root, so a pass cannot be explained by the app
/// node alone.
const APP: &str = "vet";

/// Write one generation of the projection, exactly as `just capgraph-store`
/// does: the tool's stdout, unedited, posted to `/sql`.
///
/// Not on `harness::Store` because it is the one capgraph-specific thing here —
/// the harness owns talking to SurrealDB, this owns what capgraph writes into it.
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
/// Every other test in this file starts its own container and writes both halves
/// of the join into it, so all of them passed while production had the capability
/// graph in `holon`/`holon` and the knowledge pool in `comp`/`goalmemory`. The join
/// was correct; the two halves were in different rooms.
///
/// So this asserts the COORDINATES rather than the query, by reading the places
/// that declare them. It needs no database, which is the point — the bug it guards
/// is a mismatch between files, and a test that has to boot Docker to notice is a
/// test nobody runs before shipping the mismatch.
#[test]
fn the_projection_targets_the_database_the_pool_lives_in() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();

    // What the component falls back to when `surreal-ns` is unset, and what the
    // driver rewrites the memory app's database to.
    assert!(
        read("components/knowledge-graph/src/lib.rs").contains(r#"cfg("surreal-ns", "comp")"#),
        "knowledge-graph's default namespace moved — the Justfile recipes need to move with it"
    );
    assert!(
        read("reconciler/src/bin/goalrun.rs").contains(r#"("SURREAL_DB", "goalmemory")"#),
        "comp-goalrun no longer points the pool at `goalmemory` — the projection is aimed elsewhere"
    );

    // Both recipes, because `lessons-for` reading a database `capgraph-store` never
    // wrote is the same bug wearing the other half's clothes.
    let justfile = read("Justfile");

    // By RECIPE, not by counting lines. The first version of this counted every
    // line holding `SURREAL_NS:-` and expected exactly two, so it went red the day
    // `host-console` gained a namespace — a recipe that is not wrong and is not
    // what this guards. It then went red again on `SURREAL_DB:-`, because that
    // recipe sets both, as separate `--config` flags rather than one shell line.
    //
    // What the test means is narrower: the two recipes that READ AND WRITE the pool
    // have to agree on where it is.
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

    // And nothing anywhere may default to somewhere else — a third recipe reading
    // another room is the same bug wearing different clothes.
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

/// The connections survive being written to the database.
///
/// `capgraph_edges.rs` asserts the graph is right in the tool. This asserts that
/// projecting it loses nothing: every edge lands on a node that exists, every app
/// carries its own root, and the counts the tool computed match the edges the
/// database actually holds. A projection that silently drops edges would leave the
/// join in the other test still passing on whatever survived.
#[test]
fn the_projection_preserves_the_app_and_component_connections() {
    let Some(db) = Store::start() else {
        eprintln!(
            "SKIPPED: docker could not start {SURREAL_IMAGE} — the connections are unverified"
        );
        return;
    };
    project(&db, 1);

    // 1. No edge points at a node that is not there. `out.name = NONE` is how a
    //    dangling record link reads, and it is exactly what a partial write or a
    //    mismatched id would produce.
    for edge in ["carries", "imports", "exports"] {
        let dangling =
            db.last(&format!("SELECT count() FROM {edge} WHERE out.name = NONE GROUP ALL;"));
        assert_eq!(
            dangling[0]["count"].as_u64().unwrap_or(0),
            0,
            "{edge} has edges pointing at nodes that do not exist"
        );
    }

    // 2. Every app carries its own root — the fix ADR-0091's query depends on, and
    //    the one place the projection deliberately differs from `--format json`.
    let rootless =
        db.last("SELECT VALUE name FROM app WHERE root NOT IN ->carries->artifact.name;");
    assert_eq!(
        rootless.as_array().map(|a| a.len()),
        Some(0),
        "these apps do not carry their own root, so their domain component's \
         imports are invisible to the join: {rootless:?}"
    );

    // 3. The consumer count the tool wrote on each interface agrees with the number
    //    of import edges the database holds. These are computed by different code
    //    paths — one counts in Rust, the other is the edges themselves — so a
    //    disagreement means one of them is wrong.
    let mismatched =
        db.last("SELECT VALUE name FROM interface WHERE consumers != array::len(<-imports);");
    assert_eq!(
        mismatched.as_array().map(|a| a.len()),
        Some(0),
        "consumer counts disagree with the import edges for: {mismatched:?}"
    );

    // 4. Named compositions, so a wholesale collapse cannot pass the invariants
    //    above by being uniformly empty.
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

/// The lessons the projection must never be able to touch. Deliberately tagged
/// with an interface `vet` imports and one nothing does, so the join below is
/// asserted to be selective rather than merely non-empty.
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

    // The derived half arrived. Exact counts are not asserted — they move every
    // time somebody adds a component, and a test that has to be edited for that is
    // a test people learn to edit without reading.
    for table in ["interface", "artifact", "app", "imports", "exports", "carries"] {
        assert!(db.count(table) > 0, "the projection wrote no {table} rows");
    }

    // 1. THE JOIN. Nothing in this query names CSV, or anything veterinary.
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

    // 2. THE ISOLATION. A second generation lands on top of a live pool.
    let before = db.count("memory");
    project(&db, 2);
    assert_eq!(
        db.count("memory"),
        before,
        "a rebuild changed the accumulated half — the generation stamp is not \
         holding, and the only unrecoverable data in the system is reachable from \
         the half that gets recomputed constantly"
    );

    // And the old generation is gone rather than doubled, or the projection grows
    // without bound and `consumers` counts read twice.
    let gens = db.last("SELECT VALUE array::distinct(gen) FROM interface GROUP ALL;");
    assert_eq!(
        gens[0].as_array().map(|a| a.len()),
        Some(1),
        "more than one generation of derived rows survived: {gens:?}"
    );

    // The join still answers after a rebuild — node ids are stable across
    // generations, so nothing that pointed at them dangles.
    let after = db.last(&query);
    assert_eq!(after, hits, "the join gave a different answer after a rebuild");
}

/// The lesson is reached by TRAVERSING an edge, not by scanning the pool.
///
/// ADR-0090 established that a lesson is about an interface rather than about a
/// goal's wording, and paid for it twice. ADR-0091 kept the title, merged the
/// stores, drafted `lesson -about-> interface` as a real edge — and deferred it,
/// leaving retrieval as `memory WHERE tags CONTAINSANY`, which is a full table
/// scan of the one half it measured as not scaling.
///
/// So the claim under test is narrow and checkable: the traversal finds exactly
/// what the scan finds. If it finds less, the index is lossy and the scan has to
/// stay. If it finds more, it is matching things the tags do not say.
///
/// The second half is the one that would be expensive to get wrong: `about` is a
/// DERIVED row, aged out by generation like every other edge here, and a rebuild
/// that reached a lesson while dropping its index would be the thing ADR-0091's
/// generation stamping exists to prevent.
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

    // What the SCAN finds — the old path, kept here as the reference answer.
    let scanned = db.last(&format!(
        "LET $ifaces = (SELECT VALUE array::distinct(array::flatten(\
           ->carries->artifact->imports->interface.name)) FROM ONLY app:⟨{APP}⟩);\n\
         SELECT VALUE text FROM memory WHERE tags CONTAINSANY $ifaces ORDER BY text;"
    ));

    // What the TRAVERSAL finds — the new path. Nothing in it mentions a tag.
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

    // Rebuilding drops every edge of the old generation and rebuilds it. The
    // lessons themselves must be untouched: the index is recomputable, they are not.
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

/// The index survives a pool with nothing in it, and a lesson about nothing.
///
/// Both of these broke the first two versions of the `about` projection, and
/// neither would have been caught by a test that seeded a lesson which matches:
///
///   * a database where no lesson has ever been written — every fresh install —
///     failed on `NONE` before the loop ran at all;
///   * a lesson tagged with an interface that no longer exists — which is the
///     NORMAL state under append-only interfaces (ADR-0091), since a retired
///     interface leaves its lessons behind — failed on an empty relate target.
///
/// The projection runs on every build. A statement in it that only works when the
/// data is agreeable is a statement that breaks on somebody else's machine.
#[test]
fn the_index_survives_an_empty_pool_and_a_lesson_about_nothing() {
    let Some(db) = Store::start() else {
        eprintln!(
            "SKIPPED: docker could not start {SURREAL_IMAGE} — the edge cases are unverified"
        );
        return;
    };

    // 1. Nothing has ever been learned.
    project(&db, 1);
    assert_eq!(db.count("about"), 0, "an empty pool produced edges out of nowhere");

    // 2. A lesson about an interface this repository does not have. Under
    //    append-only interfaces this is not an error state — it is what a lesson
    //    about a superseded interface looks like once the last consumer has gone.
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

    // 3. And a real one still lands, so the two above are not passing because the
    //    projection quietly stopped writing edges at all.
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
/// The derived tables answer "what is the graph"; they are rewritten whole and
/// aged out, so nothing in them can answer "did it move". `generation` is the one
/// row a projection leaves behind, and the whole claim is that the NEXT projection
/// does not take it back — which is a claim about a `DELETE` that runs seven times
/// against seven other tables and must miss this one.
///
/// Asserted against a real database rather than against the emitted SQL, because
/// the emitted SQL is where it already looks right: the unit test reads the
/// statements, and this reads what survives them.
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

    // The other half, and the reason this is not just an append test: the DERIVED
    // tables must NOT have grown. If they did, the age-out stopped working and the
    // store now answers every unfiltered query with three graphs stacked on top of
    // each other — which returns more rows rather than an error, and so looks fine.
    assert_eq!(
        db.count("artifact"),
        derived_after_one,
        "the derived half grew across generations — the age-out is not running, and \
         every query that does not filter on `gen` is now silently wrong"
    );

    // The oldest row still says what it said. An UPSERT keyed by generation can
    // only reach its own row; this is that claim, after two later builds.
    let first = db.last("SELECT gen, artifacts FROM generation WHERE gen = 1;");
    let row = first.as_array().and_then(|a| a.first().cloned()).unwrap_or_default();
    assert_eq!(
        row["artifacts"].as_u64(),
        Some(derived_after_one as u64),
        "generation 1's counts changed after later builds — the row is not immutable"
    );
}
