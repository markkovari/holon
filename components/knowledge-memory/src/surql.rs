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
    /// What the work TOUCHED, read off the capability graph rather than authored:
    /// the interfaces the part's component imports (ADR-0090). These are what make
    /// a lesson findable by a later goal that shares an interface but shares no
    /// wording — which is most of them, since a fact about `csv:codec/codec` is
    /// true for a billing ledger and a veterinary clinic alike.
    pub tags: &'a [String],
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
        "ns = {}, text = {}, goal = {}, env = {}, attempt = {}, score = {}, promoted = {}, \
         tags = [{}], uses += 0, wins += 0, last_used = time::now()",
        lit(e.ns),
        lit(e.text),
        lit(e.goal),
        lit(e.env),
        lit(e.attempt),
        e.score,
        e.promoted,
        e.tags.iter().map(|t| lit(t)).collect::<Vec<_>>().join(", "),
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

/// Recall by what the work TOUCHED rather than by what the goal SAID.
///
/// An exact match on a tag, with no embedding involved, because an interface name
/// is an identifier and not a sentence: `csv:codec/codec@0.1.0` either was imported
/// or was not. This is the half of retrieval that crosses applications — the two
/// paid runs this repository has made both died on facts about an interface that a
/// similarity search over goal text had no way to surface (ADR-0090).
///
/// `LIMIT` is generous relative to `k` because these candidates are ranked and
/// trimmed with the dense ones afterwards; taking k here would let a tag match
/// crowd out a better textual hit.
pub fn tagged_entries(tags: &[String], k: u32, pools: &[&str]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let pool_list = pools.iter().map(|p| lit(p)).collect::<Vec<_>>().join(", ");
    let tag_list = tags.iter().map(|t| lit(t)).collect::<Vec<_>>().join(", ");
    format!(
        "SELECT * FROM {ENTRIES} WHERE tags CONTAINSANY [{tag_list}] AND ns IN [{pool_list}] \
         ORDER BY wins DESC, uses DESC LIMIT {};",
        k * 3
    )
}

/// The lexical candidates the dense pass did not already return, in ONE statement.
/// One read per candidate is the N+1 of ADR-0077, and a retrieval path would do it
/// k×4 times per branch.
pub fn hydrate(handles: &[String]) -> String {
    let ids: Vec<String> = handles.iter().map(|h| rid(ENTRIES, h)).collect();
    format!("SELECT * FROM {};", ids.join(", "))
}

/// Forget what nobody used.
///
/// Two conditions, and the second one is a guard rather than a filter: an entry
/// with NO `last_used` at all would otherwise be swept, because SurrealDB
/// evaluates `NONE < time::now() - 30d` as TRUE — measured. Every write stamps the
/// field, so an unstamped row means a write path that forgot, and deleting rows
/// because of a bug in this component is the one outcome worth writing an extra
/// clause to avoid.
///
/// `uses` rather than a score: an entry two runs have read has earned its place
/// whatever it says, and one nobody has read in a month has not, whatever it
/// promised. Time is the database's own — nothing here imports a clock.
pub fn decay(max_age_days: u32, min_uses: u64) -> String {
    format!(
        "DELETE FROM {ENTRIES} WHERE uses < {min_uses} AND last_used != NONE \
         AND last_used < time::now() - {max_age_days}d RETURN BEFORE;"
    )
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
            "UPDATE {id} SET uses += 1, wins += {}, last_used = time::now();",
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

/// Record that one goal was decomposed into another.
///
/// A task -> task edge, so the pool can answer two questions it could not answer
/// before: what did this goal break into, and whose sub-goal is this. Both halves
/// matter — the first is how a decomposition is reviewed, the second is how a
/// sub-goal found later by similarity is put back in context.
///
/// Deterministic id, `<parent>|<child>`, for the same reason the verdict edge has
/// one: a run that decomposes the same goal twice reinforces one edge instead of
/// growing a fan of duplicates. Re-running a decomposed goal is the NORMAL case,
/// not an error, so this verb has to be idempotent per `(parent, child)`.
///
/// The child node is UPSERTed here with its goal text, so a sub-goal exists in the
/// pool the moment it is named — before anything has run it, and whether or not
/// anything ever does. That is what makes an abandoned decomposition legible
/// rather than invisible.
///
/// `ordinal` and `why` travel on the EDGE, not on either node: they are facts
/// about the relationship. The same sub-goal reached from two parents is one node
/// with two edges, and each edge carries its own reason.
pub fn decomposed_into(
    parent_key: &str,
    parent_goal: &str,
    child_key: &str,
    child_goal: &str,
    ordinal: u32,
    why: &str,
) -> String {
    let parent = rid(TASKS, parent_key);
    let child = rid(TASKS, child_key);
    format!(
        "BEGIN;         UPSERT {parent} SET goal = {};         UPSERT {child} SET goal = {};         RELATE {parent}->{}->{child} CONTENT {{ ordinal: {ordinal}, why: {} }};         COMMIT;",
        lit(parent_goal),
        lit(child_goal),
        part_edge(parent_key, child_key),
        lit(why),
    )
}

/// The decomposition edge's id: one per `(parent, child)`.
fn part_edge(parent: &str, child: &str) -> String {
    rid("decomposes_into", &format!("{}|{}", parent.replace('|', ""), child.replace('|', "")))
}

/// What a goal broke into, in the order it was decomposed.
///
/// Each child carries whether anything has PASSED on it, counted off its own
/// verdict edges — the same derivation `already_done` uses, so the two cannot
/// disagree about what is finished. That count is the whole point: a parent is
/// resumable only if you can tell which of its parts are already done.
pub fn parts_of(parent_key: &str) -> String {
    format!(
        "SELECT out.goal AS goal, out.id AS id, ordinal, why,          count(out->evaluated_by[WHERE passed = true]) > 0 AS done          FROM {}->decomposes_into ORDER BY ordinal;",
        rid(TASKS, parent_key)
    )
}

/// The goals this one is a part of. Plural on purpose: the same sub-goal reached
/// from two parents is one node, and hiding the second parent would make the pool
/// disagree with the edges it holds.
pub fn parents_of(child_key: &str) -> String {
    format!(
        "SELECT in.goal AS goal, in.id AS id, ordinal, why          FROM {}<-decomposes_into ORDER BY ordinal;",
        rid(TASKS, child_key)
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
const PRIOR_FIELDS: &str = "goal, score, run, artifact, count(->evaluated_by) AS evaluations";

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
    Ok(statements.last().and_then(|s| s["result"].as_array().cloned()).unwrap_or_default())
}

/// Did the database refuse this vector because the index is a different width?
/// The one rejection worth handling rather than reporting.
pub fn is_dimension_conflict(msg: &str) -> bool {
    msg.contains("vector dimension")
}
