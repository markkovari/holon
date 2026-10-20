//! The capability graph projection against a real database (ADR-0091); skipped without Docker.

use std::process::Command;

// Shared harness, included rather than copied (docs/adr/0115).
#[path = "../../tests/harness/mod.rs"]
mod harness;
use harness::{Store, SURREAL_IMAGE};

/// Reaches `csv:codec/codec` through a part, so the app node alone cannot explain a pass.
const APP: &str = "vet";

/// The tool's stdout, unedited, posted to `/sql`.
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

/// Every other test writes both halves into its own database; production once split them.
#[test]
fn the_projection_targets_the_database_the_pool_lives_in() {
    let root = comp_fleet::fleet::root_from_manifest();
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();

    assert!(
        read("components/knowledge-graph/src/lib.rs").contains(r#"cfg("surreal-ns", "comp")"#),
        "knowledge-graph's default namespace moved — the Justfile recipes need to move with it"
    );
    // The submodules too, or a refactor that moved no coordinate would go red.
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

    // Both recipes, plus the files the Justfile imports.
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

    // By recipe, not by counting lines, which broke when `host-console` gained a namespace.
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

#[test]
fn the_projection_preserves_the_app_and_component_connections() {
    let Some(db) = Store::start() else {
        eprintln!(
            "SKIPPED: docker could not start {SURREAL_IMAGE} — the connections are unverified"
        );
        return;
    };
    project(&db, 1);

    // `out.name = NONE` is a dangling record link.
    for edge in ["carries", "imports", "exports"] {
        let dangling =
            db.last(&format!("SELECT count() FROM {edge} WHERE out.name = NONE GROUP ALL;"));
        assert_eq!(
            dangling[0]["count"].as_u64().unwrap_or(0),
            0,
            "{edge} has edges pointing at nodes that do not exist"
        );
    }

    // The one place the projection differs from `--format json`.
    let rootless =
        db.last("SELECT VALUE name FROM app WHERE root NOT IN ->carries->artifact.name;");
    assert_eq!(
        rootless.as_array().map(|a| a.len()),
        Some(0),
        "these apps do not carry their own root, so their domain component's \
         imports are invisible to the join: {rootless:?}"
    );

    // Computed by different code paths, so a disagreement means one is wrong.
    let mismatched =
        db.last("SELECT VALUE name FROM interface WHERE consumers != array::len(<-imports);");
    assert_eq!(
        mismatched.as_array().map(|a| a.len()),
        Some(0),
        "consumer counts disagree with the import edges for: {mismatched:?}"
    );

    // Named compositions, so a uniformly empty collapse cannot pass.
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

    // Or Q9's staleness stamp has nothing to stamp.
    let undigested = db.last("SELECT count() FROM artifact WHERE digest = '' GROUP ALL;");
    assert_eq!(
        undigested[0]["count"].as_u64().unwrap_or(0),
        0,
        "artifacts landed without a digest — a lesson cannot record what it was \
         learned against"
    );
}

/// One tag `vet` imports and one nothing does, so the join must be selective.
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

    // Exact counts move with every new component.
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

    // Gone, not doubled, or the projection grows without bound.
    let gens = db.last("SELECT VALUE array::distinct(gen) FROM interface GROUP ALL;");
    assert_eq!(
        gens[0].as_array().map(|a| a.len()),
        Some(1),
        "more than one generation of derived rows survived: {gens:?}"
    );

    // Node ids are stable across generations.
    let after = db.last(&query);
    assert_eq!(after, hits, "the join gave a different answer after a rebuild");
}

/// The traversal must find exactly what the full scan finds, and survive a rebuild.
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

    let scanned = db.last(&format!(
        "LET $ifaces = (SELECT VALUE array::distinct(array::flatten(\
           ->carries->artifact->imports->interface.name)) FROM ONLY app:⟨{APP}⟩);\n\
         SELECT VALUE text FROM memory WHERE tags CONTAINSANY $ifaces ORDER BY text;"
    ));

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

    // The index is recomputable, the lessons are not.
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

/// A fresh install failed on `NONE`, a retired interface on an empty relate target.
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

    // Normal after a retirement (ADR-0091).
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

    // So the above are not passing because the projection stopped writing edges.
    db.last(SEED);
    project(&db, 3);
    assert!(
        db.count("about") > 0,
        "no edges after seeding real lessons — the projection writes nothing, and the \
         two assertions above prove nothing"
    );
}

/// `generation` is the one row a rebuild leaves; a `DELETE` on the derived tables must miss it.
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

    // Grown would mean the age-out stopped and queries return stacked graphs.
    assert_eq!(
        db.count("artifact"),
        derived_after_one,
        "the derived half grew across generations — the age-out is not running, and \
         every query that does not filter on `gen` is now silently wrong"
    );

    // An UPSERT keyed by generation only reaches its own row.
    let first = db.last("SELECT gen, artifacts FROM generation WHERE gen = 1;");
    let row = first.as_array().and_then(|a| a.first().cloned()).unwrap_or_default();
    assert_eq!(
        row["artifacts"].as_u64(),
        Some(derived_after_one),
        "generation 1's counts changed after later builds — the row is not immutable"
    );
}
