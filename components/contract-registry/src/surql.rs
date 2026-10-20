//! Plain strings, so a live scenario suite runs these exact statements.

use serde_json::Value;

pub const CONTRACTS: &str = "contract";
pub const REQUESTS: &str = "request";
pub const BUILDS: &str = "build";

/// `n += 1 RETURN n` is atomic; a read-modify-write landed 7 of 60 concurrent increments (ADR-0084).
pub const SEQ: &str = "contract_seq";

pub fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

pub fn lit(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

pub fn next_version() -> String {
    format!("UPSERT {}:⟨1⟩ SET n += 1 RETURN n;", SEQ)
}

/// `canonical` is false for an amendment awaiting its author's gate.
pub fn put_contract(
    version: u32,
    body: &str,
    canonical: bool,
    owner: &str,
    from_request: &str,
) -> String {
    format!(
        "UPSERT {} SET version = {version}, body = {}, canonical = {canonical}, owner = {}, from_request = {};",
        rid(CONTRACTS, &version.to_string()),
        lit(body),
        lit(owner),
        lit(from_request)
    )
}

/// The highest ratified version: an unimplemented amendment must never be what siblings build on.
pub fn current_contract() -> String {
    format!("SELECT * FROM {CONTRACTS} WHERE canonical = true ORDER BY version DESC LIMIT 1;")
}

/// `SELECT *`: SurrealDB rejects ORDER BY over a field the projection omits.
pub fn proposed_for(part: &str) -> String {
    format!(
        "SELECT * FROM {CONTRACTS} WHERE canonical = false AND owner = {} ORDER BY version DESC LIMIT 1;",
        lit(part)
    )
}

pub fn get_contract(version: u32) -> String {
    format!("SELECT * FROM {};", rid(CONTRACTS, &version.to_string()))
}

/// Guarded in the statement, so a part cannot ratify a version it does not own, even racing.
pub fn ratify(version: u32, part: &str) -> String {
    format!(
        "UPDATE {} SET canonical = true WHERE owner = {} RETURN version;",
        rid(CONTRACTS, &version.to_string()),
        lit(part)
    )
}

pub fn put_request(
    id: &str,
    from_part: &str,
    to_part: &str,
    subject: &str,
    body: &str,
    at_version: u32,
) -> String {
    // Deterministic id, so a retried ask reinforces one row (ADR-0084). `answered` is not set: false on
    // every ask would un-answer a verdict already given.
    format!(
        "BEGIN;UPSERT {} SET from_part = {}, to_part = {}, subject = {}, body = {}, \
         at_version = {at_version};\
         RELATE {}->asked_of->{} CONTENT {{ subject: {} }};COMMIT;",
        rid(REQUESTS, id),
        lit(from_part),
        lit(to_part),
        lit(subject),
        lit(body),
        rid(REQUESTS, id),
        rid("part", to_part),
        lit(subject)
    )
}

/// `SELECT *` is load-bearing: the ORDER BY field must be projected or the statement fails.
pub fn pending(to_part: &str) -> String {
    format!(
        "SELECT * FROM {REQUESTS} WHERE to_part = {} AND answered != true ORDER BY at_version;",
        lit(to_part)
    )
}

pub fn get_request(id: &str) -> String {
    format!("SELECT * FROM {};", rid(REQUESTS, id))
}

/// `WHERE answered = false` guards in the statement, so a racing second answer cannot overwrite.
pub fn answer(id: &str, verdict: &str, answer: &str, version: u32) -> String {
    format!(
        "UPDATE {} SET answered = true, verdict = {}, answer = {}, amended = {version} \
         WHERE answered != true RETURN id;",
        rid(REQUESTS, id),
        lit(verdict),
        lit(answer)
    )
}

/// The edge makes "which decision broke the join" a traversal.
pub fn built_against(candidate: &str, part: &str, version: u32) -> String {
    format!(
        "BEGIN;UPSERT {} SET part = {}, version = {version};\
         RELATE {}->built_against->{} CONTENT {{ part: {} }};COMMIT;",
        rid(BUILDS, candidate),
        lit(part),
        rid(BUILDS, candidate),
        rid(CONTRACTS, &version.to_string()),
        lit(part)
    )
}

pub fn builds(candidates: &[String]) -> String {
    let ids: Vec<String> = candidates.iter().map(|c| rid(BUILDS, c)).collect();
    format!("SELECT * FROM {};", ids.join(", "))
}

/// `Ok(empty)` for an unwritten table or namespace: the first `current()` precedes the first `publish`.
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

pub fn request_id(from_part: &str, to_part: &str, subject: &str, at_version: u32) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!("{from_part}|{to_part}|{subject}|{at_version}").as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Names both sides: "not composable" alone sends the reader to the wrong file.
pub fn disagreements(builds: &[(String, String, u32)]) -> Vec<String> {
    let Some((_, _, agreed)) = builds.first() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (candidate, part, version) in builds.iter().skip(1) {
        if version != agreed {
            let (first_candidate, first_part, _) = &builds[0];
            out.push(format!(
                "{part} ({candidate}) built against contract v{version}, \
                 {first_part} ({first_candidate}) against v{agreed}"
            ));
        }
    }
    out
}

pub fn contract_of(row: &Value) -> (u32, String, bool, String, String) {
    (
        row["version"].as_u64().unwrap_or(0) as u32,
        row["body"].as_str().unwrap_or_default().to_string(),
        row["canonical"].as_bool().unwrap_or(false),
        row["owner"].as_str().unwrap_or_default().to_string(),
        row["from_request"].as_str().unwrap_or_default().to_string(),
    )
}

pub fn id_of(row: &Value) -> String {
    row["id"]
        .as_str()
        .and_then(|full| full.split_once(':'))
        .map(|(_, id)| id.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩').to_string())
        .unwrap_or_default()
}
