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
use std::time::Duration;

use serde_json::Value;

mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

/// The app the join is asserted against. It reaches `csv:codec/codec` through a
/// part rather than through its own root, so a pass cannot be explained by the app
/// node alone.
const APP: &str = "vet";

/// A SurrealDB from the shared harness, plus the two things this file adds: a way
/// to run the projection binary into it, and a reader for the last statement's
/// result.
///
/// The container, the pinned image, the free port and the `Drop` that reclaims it
/// all come from `harness::Surreal`, which every other database test in this suite
/// already uses.
struct Store {
    db: Surreal,
    http: reqwest::blocking::Client,
}

impl Store {
    fn start() -> Option<Self> {
        let db = Surreal::start()?;
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap();
        let me = Self { db, http };
        me.raw("DEFINE NAMESPACE IF NOT EXISTS holon;");
        me.raw("DEFINE DATABASE IF NOT EXISTS holon;");
        Some(me)
    }

    fn raw(&self, body: &str) -> Vec<Value> {
        let text = self
            .http
            .post(format!("http://127.0.0.1:{}/sql", self.db.port))
            .basic_auth("root", Some(SURREAL_PASSWORD))
            .header("accept", "application/json")
            .header("surreal-ns", "holon")
            .header("surreal-db", "holon")
            .body(body.to_string())
            .send()
            .and_then(|r| r.text())
            .unwrap_or_default();
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// The result of the LAST statement, with every statement checked. A rejected
    /// statement in the middle of a projection is the failure mode that would
    /// otherwise show up as a mysteriously empty query later.
    fn last(&self, body: &str) -> Value {
        let answered = self.raw(body);
        let failed: Vec<&Value> = answered.iter().filter(|s| s["status"] != "OK").collect();
        assert!(failed.is_empty(), "{} statement(s) rejected: {:?}", failed.len(), failed);
        answered.last().map(|s| s["result"].clone()).unwrap_or(Value::Null)
    }

    fn count(&self, table: &str) -> u64 {
        self.last(&format!("SELECT count() FROM {table} GROUP ALL;"))[0]["count"]
            .as_u64()
            .unwrap_or(0)
    }

    /// Write one generation of the projection, exactly as `just capgraph-store`
    /// does: the tool's stdout, unedited, posted to `/sql`.
    fn project(&self, generation: u64) {
        let out = Command::new(env!("CARGO_BIN_EXE_comp-capgraph"))
            .args(["--format", "surql", "--gen", &generation.to_string()])
            .output()
            .expect("comp-capgraph did not run");
        assert!(
            out.status.success(),
            "comp-capgraph failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let sql = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(sql.contains("UPSERT interface:"), "projection wrote no interfaces");
        self.last(&sql);
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
        eprintln!("SKIPPED: docker could not start {SURREAL_IMAGE} — the connections are unverified");
        return;
    };
    db.project(1);

    // 1. No edge points at a node that is not there. `out.name = NONE` is how a
    //    dangling record link reads, and it is exactly what a partial write or a
    //    mismatched id would produce.
    for edge in ["carries", "imports", "exports"] {
        let dangling = db.last(&format!(
            "SELECT count() FROM {edge} WHERE out.name = NONE GROUP ALL;"
        ));
        assert_eq!(
            dangling[0]["count"].as_u64().unwrap_or(0),
            0,
            "{edge} has edges pointing at nodes that do not exist"
        );
    }

    // 2. Every app carries its own root — the fix ADR-0091's query depends on, and
    //    the one place the projection deliberately differs from `--format json`.
    let rootless = db.last(
        "SELECT VALUE name FROM app WHERE root NOT IN ->carries->artifact.name;",
    );
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
        let has = carried
            .as_array()
            .map(|a| a.iter().any(|x| x.as_str() == Some(part)))
            .unwrap_or(false);
        assert!(has, "{app} does not carry {part} in the store: {carried:?}");
    }

    // 5. Every artifact has a digest, or Q9's staleness stamp has nothing to stamp.
    let undigested =
        db.last("SELECT count() FROM artifact WHERE digest = '' GROUP ALL;");
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

    db.project(1);

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
    let texts: Vec<&str> = hits.as_array().map(|a| {
        a.iter().filter_map(|h| h["text"].as_str()).collect()
    }).unwrap_or_default();

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
    db.project(2);
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
