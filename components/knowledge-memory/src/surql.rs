//! Every statement this component sends, and the parser for what comes back.
//!
//! Separate from `lib.rs` because it takes and returns PLAIN Rust types, so `scenarios.rs` can run these
//! exact strings against a real SurrealDB. Everything a caller supplies is quoted or restricted on the
//! way in: ids get SurrealDB's angle-bracket form with the closing bracket removed, and text goes through
//! JSON re-serialisation so a value cannot carry syntax (ADR-0080).

use serde_json::{json, Value};

/// Entries live in one table with their namespace a property, so a handle addresses an entry on its own.
pub const ENTRIES: &str = "memory";

/// Evaluated goals: a different table because it answers "has this been done?".
pub const TASKS: &str = "task";

/// A record id. Angle brackets are SurrealDB's quoting for an arbitrary id; the closing bracket is the one
/// character that could end quoting early, so it is removed. Caller-supplied keys reach here: a trust boundary.
pub fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

/// A SurrealQL string literal, via JSON: every escape SurrealQL needs JSON needs too, and re-serialising stops a value carrying syntax.
pub fn lit(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

pub fn vec_lit(v: &[f32]) -> String {
    json!(v).to_string()
}

/// `DEFINE INDEX IF NOT EXISTS` beside the write. Idempotent, ~1ms, and it makes a change of embedding
/// model a rejection rather than a silent mixture: the index carries its width.
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
    /// What the work TOUCHED, read off the capability graph rather than authored: the interfaces the part's
    /// component imports (ADR-0090). Makes a lesson findable by a goal that shares an interface but no wording.
    pub tags: &'a [String],
}

/// Store an entry, with or without its vector.
///
/// One statement, no read first: `uses += 0` creates the counter or preserves it, so re-observing keeps
/// earned standing without a read-modify-write (which lost 88% of writes under concurrency, ADR-0084).
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

/// Recall by what the work TOUCHED rather than by what the goal SAID: an exact tag match, no embedding,
/// because an interface name is an identifier, not a sentence. This is the half of retrieval that crosses
/// applications (ADR-0090).
///
/// `LIMIT` is generous relative to `k` because candidates are ranked and trimmed with the dense ones
/// afterwards; taking k here could let a tag match crowd out a better textual hit.
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

/// The lexical candidates the dense pass did not already return, in ONE statement (the N+1 of ADR-0077).
pub fn hydrate(handles: &[String]) -> String {
    let ids: Vec<String> = handles.iter().map(|h| rid(ENTRIES, h)).collect();
    format!("SELECT * FROM {};", ids.join(", "))
}

/// Forget what nobody used.
///
/// The second condition is a guard, not a filter: an entry with NO `last_used` would be swept because
/// SurrealDB evaluates `NONE < time::now() - 30d` as TRUE (measured). Every write stamps the field, so an
/// unstamped row means a buggy write path, and deleting rows over that is the outcome to avoid.
/// Time is the database's own; nothing here imports a clock.
pub fn decay(max_age_days: u32, min_uses: u64) -> String {
    format!(
        "DELETE FROM {ENTRIES} WHERE uses < {min_uses} AND last_used != NONE \
         AND last_used < time::now() - {max_age_days}d RETURN BEFORE;"
    )
}

/// A whole verdict in one transaction.
///
/// `UPDATE` not `UPSERT`: a handle a human deleted stays deleted rather than resurrected with counters and
/// no lesson. The `used_in` edges are the record; the counters a denormalised read, so both land under one commit.
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
/// The verdict is an EDGE with a deterministic id, `<task>|<run>`; `RELATE` with an explicit id is an
/// upsert (measured on v3.1.3), so this verb is idempotent per `(goal, run)` and the landing path can call
/// it again to attach the pull request. Counts are DERIVED from the edges (see `already_done_*`), not
/// stored: "exactly once" across a fan-out of branches is broken by a retry.
///
/// Only a passing verdict overwrites the winner fields. A goal five runs have failed is knowledge too (the
/// count says whether a sixth attempt is worth buying), but it is not finished work.
// One over clippy's default threshold, each a distinct scalar this query
// binds; a params struct would only exist to satisfy the lint.
#[allow(clippy::too_many_arguments)]
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
/// A task -> task edge, so the pool can say what a goal broke into and whose sub-goal this is. Deterministic
/// id, `<parent>|<child>`, so re-running a decomposed goal (the NORMAL case) reinforces one edge.
///
/// The child node is UPSERTed with its goal text, so a sub-goal exists the moment it is named, which makes
/// an abandoned decomposition legible. `ordinal` and `why` travel on the EDGE: they describe the
/// relationship, and one sub-goal under two parents has two edges, each with its own reason.
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

/// What a goal broke into, in the order it was decomposed. Each child carries whether anything has PASSED
/// on it, counted off its verdict edges (as `already_done` does), so a parent is resumable.
pub fn parts_of(parent_key: &str) -> String {
    format!(
        "SELECT out.goal AS goal, out.id AS id, ordinal, why,          count(out->evaluated_by[WHERE passed = true]) > 0 AS done          FROM {}->decomposes_into ORDER BY ordinal;",
        rid(TASKS, parent_key)
    )
}

/// The goals this one is a part of. Plural on purpose: one sub-goal under two parents is one node.
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

/// `evaluations` and `passes` are counted off the edges rather than read off the node, which makes a re-report free.
///
/// ponytail: a traversal per candidate where a stored integer would be one read. Irrelevant at this pool
/// size; the fix is a materialised count maintained by the statement that writes the edge.
const PRIOR_FIELDS: &str = "goal, score, run, artifact, count(->evaluated_by) AS evaluations";

/// A goal counts as done when at least one verdict on it passed (a count over EDGES, so it cannot disagree with the trail).
const HAS_PASSED: &str = "count(->evaluated_by[WHERE passed = true]) > 0";

/// Nearest passing goal, computed in the database. "At least one verdict passed", not "the last one": a
/// goal that passed once and failed twice is still done work. It returns the nearest qualifying row even
/// when nothing is near (measured), so the caller's similarity floor is what makes the answer correct.
pub fn already_done_knn(query: &[f32]) -> String {
    format!(
        "SELECT {PRIOR_FIELDS}, vector::distance::knn() AS dist FROM {TASKS} \
         WHERE vec <|1,COSINE|> {} AND {HAS_PASSED};",
        vec_lit(query)
    )
}

/// The same question with no embedding provider linked: an exact match on the normalised goal.
pub fn already_done_exact(key: &str) -> String {
    format!("SELECT {PRIOR_FIELDS} FROM {} WHERE {HAS_PASSED};", rid(TASKS, key))
}

/// `vector::distance::knn()` answers a COSINE DISTANCE (0.0 for an exact match) but every threshold here is
/// a similarity, so converting in one named place keeps the floor from keeping everything.
pub fn similarity_of(distance: f64) -> f64 {
    1.0 - distance
}

/// The rows a statement answered with, or the database's own words.
///
/// `Ok(empty)` covers a table or namespace nobody has defined yet (both answer "does not exist"; the first
/// `recall` precedes the first write). An error in ANY statement of the body is an error, since inside
/// `BEGIN`/`COMMIT` one failed statement takes the transaction down.
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

/// Did the database refuse this vector because the index is a different width? The one rejection worth handling.
pub fn is_dimension_conflict(msg: &str) -> bool {
    msg.contains("vector dimension")
}
