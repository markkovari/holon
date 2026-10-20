//! Every statement, as plain strings so scenarios.rs runs them on a real SurrealDB (ADR-0080).

use serde_json::{json, Value};

pub const ENTRIES: &str = "memory";

pub const TASKS: &str = "task";

/// Trust boundary: `>` is the one character that ends angle-bracket quoting early, so it is removed.
pub fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

/// Via JSON: SurrealQL needs the same escapes, and re-serialising stops a value carrying syntax.
pub fn lit(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

pub fn vec_lit(v: &[f32]) -> String {
    json!(v).to_string()
}

/// The index carries its width, so a model change is rejected rather than silently mixed.
pub fn define_index(table: &str, dim: usize) -> String {
    format!(
        "DEFINE INDEX IF NOT EXISTS {table}_vec ON {table} FIELDS vec HNSW DIMENSION {dim} DIST COSINE;"
    )
}

pub struct EntryWrite<'a> {
    pub handle: &'a str,
    pub ns: &'a str,
    pub text: &'a str,
    pub goal: &'a str,
    pub env: &'a str,
    pub attempt: &'a str,
    pub score: i32,
    pub promoted: bool,
    /// The part component's imported interfaces (ADR-0090): findable by a goal sharing no wording.
    pub tags: &'a [String],
}

/// `uses += 0` creates or preserves the counter; read-modify-write lost 88% of writes (ADR-0084).
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

pub fn knn_entries(query: &[f32], k: u32, pools: &[&str]) -> String {
    let pool_list = pools.iter().map(|p| lit(p)).collect::<Vec<_>>().join(", ");
    format!(
        "SELECT *, vector::distance::knn() AS dist FROM {ENTRIES} \
         WHERE vec <|{k},COSINE|> {} AND ns IN [{pool_list}];",
        vec_lit(query)
    )
}

/// LIMIT is generous: candidates are trimmed with the dense ones, so a tag match must not crowd
/// out a better textual hit.
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

/// One statement, not N+1 (ADR-0077).
pub fn hydrate(handles: &[String]) -> String {
    let ids: Vec<String> = handles.iter().map(|h| rid(ENTRIES, h)).collect();
    format!("SELECT * FROM {};", ids.join(", "))
}

/// The `last_used` guard is not a filter: SurrealDB evaluates `NONE < time::now() - 30d` as TRUE
/// (measured), so an unstamped row would be swept.
pub fn decay(max_age_days: u32, min_uses: u64) -> String {
    format!(
        "DELETE FROM {ENTRIES} WHERE uses < {min_uses} AND last_used != NONE \
         AND last_used < time::now() - {max_age_days}d RETURN BEFORE;"
    )
}

/// `UPDATE` not `UPSERT`: a handle a human deleted stays deleted.
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

/// Idempotent per (goal, run): `RELATE` with an explicit id upserts (v3.1.3); counts derive from edges.
/// Only a passing verdict overwrites the winner fields.
// One over clippy's threshold; a params struct would exist only for the lint.
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

/// Deterministic `<parent>|<child>` id, so re-running a decomposed goal reinforces one edge.
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

fn part_edge(parent: &str, child: &str) -> String {
    rid("decomposes_into", &format!("{}|{}", parent.replace('|', ""), child.replace('|', "")))
}

pub fn parts_of(parent_key: &str) -> String {
    format!(
        "SELECT out.goal AS goal, out.id AS id, ordinal, why,          count(out->evaluated_by[WHERE passed = true]) > 0 AS done          FROM {}->decomposes_into ORDER BY ordinal;",
        rid(TASKS, parent_key)
    )
}

pub fn parents_of(child_key: &str) -> String {
    format!(
        "SELECT in.goal AS goal, in.id AS id, ordinal, why          FROM {}<-decomposes_into ORDER BY ordinal;",
        rid(TASKS, child_key)
    )
}

fn verdict_edge(key: &str, run: &str) -> String {
    rid("evaluated_by", &format!("{}|{}", key.replace('|', ""), run.replace('|', "")))
}

/// ponytail: a traversal per candidate instead of a stored count; materialise it if the pool grows.
const PRIOR_FIELDS: &str = "goal, score, run, artifact, count(->evaluated_by) AS evaluations";

const HAS_PASSED: &str = "count(->evaluated_by[WHERE passed = true]) > 0";

/// Returns the nearest passing row even when nothing is near (measured); the caller's floor decides.
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

/// `knn()` answers a cosine distance but every threshold here is a similarity.
pub fn similarity_of(distance: f64) -> f64 {
    1.0 - distance
}

/// `Ok(empty)` for an undefined table (the first recall precedes the first write); any failed
/// statement fails the whole `BEGIN`/`COMMIT`.
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

pub fn is_dimension_conflict(msg: &str) -> bool {
    msg.contains("vector dimension")
}
