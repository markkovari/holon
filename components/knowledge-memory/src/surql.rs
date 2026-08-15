//! Every statement this component sends, and the parser for what comes back.
//!
//! Separate from `lib.rs` for one reason: it takes and returns PLAIN Rust types,
//! no generated bindings anywhere in its signatures. That is what lets the
//! scenario suite (`scenarios.rs`) run these exact strings against a real
//! SurrealDB and assert what the database answers. A scenario suite that rebuilt
//! the statements to test them would be testing itself.
//!
//! Everything a caller supplies is quoted or restricted on the way in: ids get
//! SurrealDB's angle-bracket form with the closing bracket removed, and text goes
//! through JSON re-serialisation so a value cannot carry syntax (ADR-0080).

use serde_json::{json, Value};

/// Entries live in one table with their namespace a property, so a handle
/// addresses an entry on its own.
pub const ENTRIES: &str = "memory";

/// Evaluated goals. A different table because it answers a different question —
/// "has this been done?" rather than "what should I read?".
pub const TASKS: &str = "task";

/// A record id. The angle brackets are SurrealDB's own quoting for an arbitrary
/// id, and the closing bracket is the one character that could end the quoting
/// early, so it is removed. Caller-supplied keys reach here, which makes this a
/// trust boundary rather than a formatting detail.
pub fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

/// A SurrealQL string literal, via JSON. Every escape SurrealQL needs is one JSON
/// needs too, and re-serialising is how a value stops being able to carry syntax.
pub fn lit(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

pub fn vec_lit(v: &[f32]) -> String {
    json!(v).to_string()
}

/// `DEFINE INDEX IF NOT EXISTS` beside the write. Idempotent, ~1ms, and it is what
/// makes a change of embedding model a rejection rather than a silent mixture: the
/// index carries its width and SurrealDB refuses any other.
pub fn define_index(table: &str, dim: usize) -> String {
    format!(
        "DEFINE INDEX IF NOT EXISTS {table}_vec ON {table} FIELDS vec HNSW DIMENSION {dim} DIST COSINE;"
    )
}

/// What a write knows about an entry. Plain strings, because this module is the
/// boundary the scenario suite reuses.
pub struct EntryWrite<'a> {
    pub handle: &'a str,
    pub ns: &'a str,
    pub text: &'a str,
    pub goal: &'a str,
    pub env: &'a str,
    pub attempt: &'a str,
    pub score: i32,
    pub promoted: bool,
}

/// Store an entry, with or without its vector.
///
/// One statement, and no read first. `uses += 0` creates the counter when it is
/// absent and preserves it when it is not, which is how re-observing a lesson
/// keeps the standing it earned without this component ever holding a counter in
/// a variable — the read-modify-write that does that lost 88% of its writes under
/// concurrency (ADR-0084).
pub fn upsert_entry(e: &EntryWrite, vector: Option<&[f32]>, dim_conflict: bool) -> String {
    let id = rid(ENTRIES, e.handle);
    let mut set = format!(
        "ns = {}, text = {}, goal = {}, env = {}, attempt = {}, score = {}, promoted = {}, uses += 0, wins += 0",
        lit(e.ns),
        lit(e.text),
        lit(e.goal),
        lit(e.env),
        lit(e.attempt),
        e.score,
        e.promoted,
    );
    let mut define = String::new();
    match vector {
        Some(v) => {
            define = define_index(ENTRIES, v.len());
            set.push_str(&format!(", vec = {}, dim = {}", vec_lit(v), v.len()));
        }
        None if dim_conflict => set.push_str(", dim_conflict = true"),
        None => {}
    }
    format!("{define}UPSERT {id} SET {set};")
}

/// Dense recall, computed in the database. Whole rows come back, so these
/// candidates need no hydration afterwards.
pub fn knn_entries(query: &[f32], k: u32, pools: &[&str]) -> String {
    let pool_list = pools.iter().map(|p| lit(p)).collect::<Vec<_>>().join(", ");
    format!(
        "SELECT *, vector::distance::knn() AS dist FROM {ENTRIES} \
         WHERE vec <|{k},COSINE|> {} AND ns IN [{pool_list}];",
        vec_lit(query)
    )
}

/// The lexical candidates the dense pass did not already return, in ONE statement.
/// One read per candidate is the N+1 of ADR-0077, and a retrieval path would do it
/// k×4 times per branch.
pub fn hydrate(handles: &[String]) -> String {
    let ids: Vec<String> = handles.iter().map(|h| rid(ENTRIES, h)).collect();
    format!("SELECT * FROM {};", ids.join(", "))
}

/// A whole verdict in one transaction.
///
/// `UPDATE` and not `UPSERT`, deliberately: a handle a human deleted stays
/// deleted, where `UPSERT` would resurrect it as a node with counters and no
/// lesson. The `used_in` edges are the record; the counters are a denormalised
/// read of them, which is why both land under one commit.
pub fn attribute(handles: &[String], run: &str, succeeded: bool) -> String {
    let mut stmt = String::from("BEGIN;");
    for h in handles {
        let id = rid(ENTRIES, h);
        stmt.push_str(&format!(
            "UPDATE {id} SET uses += 1, wins += {};",
            u8::from(succeeded)
        ));
        stmt.push_str(&format!(
            "RELATE {id}->used_in->{} CONTENT {{ succeeded: {succeeded} }};",
            rid("run", run)
        ));
    }
    stmt.push_str("COMMIT;");
    stmt
}

/// Record that a goal was evaluated, whatever the verdict.
///
/// The verdict is an EDGE with a deterministic id, `<task>|<run>`, and re-reporting
/// the same run overwrites that one edge rather than adding another — measured
/// against v3.1.3, where `RELATE` with an explicit id is an upsert and not a
/// duplicate-key error. So this verb is idempotent per `(goal, run)`, which is
/// what lets the landing path call it a second time to attach the pull request
/// once the forge has opened one, without inventing a second evaluation.
///
/// Counts are therefore DERIVED from the edges (see `already_done_*`) and are not
/// stored. A counter would have had to be bumped exactly once per run, and
/// "exactly once" across a fan-out of branches that each report is the kind of
/// promise that is broken by a retry.
///
/// Only a passing verdict overwrites the winner fields. A goal five runs have
/// failed is knowledge too — it is the count that says whether a sixth attempt is
/// worth buying — but it is not finished work.
pub fn evaluated(
    key: &str,
    goal: &str,
    run: &str,
    score: i32,
    passed: bool,
    artifact: &str,
    vector: Option<&[f32]>,
    dim_conflict: bool,
) -> String {
    let id = rid(TASKS, key);
    let mut set = format!("goal = {}", lit(goal));
    if passed {
        set.push_str(&format!(
            ", score = {score}, run = {}, artifact = {}",
            lit(run),
            lit(artifact)
        ));
    }
    let mut define = String::new();
    match vector {
        Some(v) => {
            define = define_index(TASKS, v.len());
            set.push_str(&format!(", vec = {}, dim = {}", vec_lit(v), v.len()));
        }
        None if dim_conflict => set.push_str(", dim_conflict = true"),
        None => {}
    }
    format!(
        "{define}BEGIN;UPSERT {id} SET {set};\
         RELATE {id}->{}->{} CONTENT {{ score: {score}, passed: {passed} }};COMMIT;",
        verdict_edge(key, run),
        rid("run", run)
    )
}

/// The verdict edge's id: one per `(task, run)`, so a run that reports twice
/// reinforces one edge instead of counting twice.
fn verdict_edge(key: &str, run: &str) -> String {
    rid("evaluated_by", &format!("{}|{}", key.replace('|', ""), run.replace('|', "")))
}

/// `evaluations` is counted off the edges rather than read off the node, which is
/// what makes a re-report free. `passes` is counted the same way and is spent in
/// the WHERE clause rather than returned.
///
/// ponytail: a traversal per candidate where a stored integer would be one read.
/// Irrelevant at a pool this size, and the moment it is not, the fix is a
/// materialised count maintained by the same statement that writes the edge.
const PRIOR_FIELDS: &str =
    "goal, score, run, artifact, count(->evaluated_by) AS evaluations";

/// A goal counts as done when at least one verdict on it passed. Note this is a
/// count over EDGES, so it cannot disagree with the trail the way a counter can.
const HAS_PASSED: &str = "count(->evaluated_by[WHERE passed = true]) > 0";

/// Nearest passing goal, computed in the database.
///
/// "At least one verdict passed", not "the last verdict passed": a goal that
/// passed once and failed twice is still done work. Note that this returns the
/// nearest row that qualifies even when nothing is near — measured, and asked
/// about an unrelated goal it will happily answer with something orthogonal. The
/// caller's similarity floor is what makes the answer correct, not the query.
pub fn already_done_knn(query: &[f32]) -> String {
    format!(
        "SELECT {PRIOR_FIELDS}, vector::distance::knn() AS dist FROM {TASKS} \
         WHERE vec <|1,COSINE|> {} AND {HAS_PASSED};",
        vec_lit(query)
    )
}

/// The same question with no embedding provider linked: an exact match on the
/// normalised goal. It still catches the duplicated work that dominates — a
/// retried generation asking for the same thing again.
pub fn already_done_exact(key: &str) -> String {
    format!("SELECT {PRIOR_FIELDS} FROM {} WHERE {HAS_PASSED};", rid(TASKS, key))
}

/// `vector::distance::knn()` answers a COSINE DISTANCE — 0.0 for an exact match —
/// and every threshold in this design is a similarity. Converting in one named
/// place is the difference between a floor that filters and one that keeps
/// everything.
pub fn similarity_of(distance: f64) -> f64 {
    1.0 - distance
}

/// The rows a statement answered with, or the database's own words.
///
/// `Ok(empty)` covers the two shapes that are not failures: a table nobody has
/// written yet and a namespace nobody has defined yet both answer "does not
/// exist", and the first `recall` of a project always precedes its first write.
///
/// An error in ANY statement of the body is an error, not just the last one:
/// inside `BEGIN`/`COMMIT` one failed statement takes the whole transaction down.
pub fn rows(body: &str) -> Result<Vec<Value>, String> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| format!("unreadable answer from the graph: {e}"))?;
    let statements = v.as_array().cloned().unwrap_or_default();
    if let Some(msg) = statements
        .iter()
        .find(|s| s["status"].as_str().unwrap_or("OK") != "OK")
        .map(|s| s["result"].as_str().unwrap_or("the statement failed").to_string())
    {
        if msg.contains("does not exist") {
            return Ok(Vec::new());
        }
        return Err(msg);
    }
    Ok(statements
        .last()
        .and_then(|s| s["result"].as_array().cloned())
        .unwrap_or_default())
}

/// Did the database refuse this vector because the index is a different width?
/// The one rejection worth handling rather than reporting.
pub fn is_dimension_conflict(msg: &str) -> bool {
    msg.contains("vector dimension")
}
