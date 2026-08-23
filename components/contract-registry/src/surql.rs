//! Every statement this component sends, and the readers for what comes back.
//!
//! Plain types only, no generated bindings in any signature — the same split
//! `knowledge-memory` uses, and for the same reason: a live scenario suite can run
//! these exact strings, and a suite that rebuilt them would be testing itself.

use serde_json::Value;

pub const CONTRACTS: &str = "contract";
pub const REQUESTS: &str = "request";
pub const BUILDS: &str = "build";

/// The version counter. A singleton row, incremented in the database.
///
/// NOT read-then-write: two parts granting amendments at the same generation
/// boundary is the normal case here, and a read-modify-write on a hot key was
/// measured landing 7 of 60 concurrent increments (ADR-0084). `n += 1 RETURN n`
/// is atomic, and `knowledge:graph` resends it if the transaction conflicts.
pub const SEQ: &str = "contract_seq";

pub fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

pub fn lit(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

/// Claim the next version number, atomically.
pub fn next_version() -> String {
    format!("UPSERT {}:⟨1⟩ SET n += 1 RETURN n;", SEQ)
}

/// Write a version. `canonical` is false for an amendment awaiting its author's
/// gate, true for the human's first contract.
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

/// The latest CANONICAL version — what a part builds against.
///
/// A proposed amendment nobody has implemented must never be what siblings build
/// on, so the filter is not "the highest version" but "the highest ratified one".
pub fn current_contract() -> String {
    format!("SELECT * FROM {CONTRACTS} WHERE canonical = true ORDER BY version DESC LIMIT 1;")
}

/// The latest version a part has granted and not yet demonstrated.
///
/// `SELECT *` because the sort is on a projected field — SurrealDB rejects an
/// `ORDER BY` over anything the projection omits.
pub fn proposed_for(part: &str) -> String {
    format!(
        "SELECT * FROM {CONTRACTS} WHERE canonical = false AND owner = {} ORDER BY version DESC LIMIT 1;",
        lit(part)
    )
}

pub fn get_contract(version: u32) -> String {
    format!("SELECT * FROM {};", rid(CONTRACTS, &version.to_string()))
}

/// Make a proposed version canonical. Guarded in the statement rather than after a
/// read: `WHERE owner = …` means a part cannot ratify a version it does not own
/// even if two ratifications race.
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
    // The edge is the history: who asked whom. Deterministic id, so a retried
    // `ask` reinforces one row rather than asking twice (ADR-0084).
    //
    // `answered` is NOT set here, which is load-bearing. Setting it to false on
    // every ask means a part that asks the same thing again — which it will, every
    // generation, until the contract moves — silently un-answers the verdict it
    // was already given, and the answering model is paid to make the same decision
    // for ever. An absent field reads as unanswered (`!= true`) and an answered one
    // stays answered.
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

/// What a part has been asked and has not answered.
///
/// `SELECT *` is load-bearing, not laziness: SurrealDB requires the `ORDER BY`
/// field to be IN THE PROJECTION, and answers an explicit projection that omits it
/// with *"Missing order idiom `at_version` in statement selection"* — a parse
/// error, so the whole statement fails rather than the ordering being ignored.
/// Narrowing these columns later would break the sort, at a distance, with an
/// error about parsing.
pub fn pending(to_part: &str) -> String {
    format!(
        "SELECT * FROM {REQUESTS} WHERE to_part = {} AND answered != true ORDER BY at_version;",
        lit(to_part)
    )
}

pub fn get_request(id: &str) -> String {
    format!("SELECT * FROM {};", rid(REQUESTS, id))
}

/// Answer one, and refuse to answer it twice.
///
/// `WHERE answered = false` is the guard, in the statement: two boundary passes
/// resolving the same request would otherwise both succeed and the second would
/// silently overwrite the first part's verdict.
pub fn answer(id: &str, verdict: &str, answer: &str, version: u32) -> String {
    format!(
        "UPDATE {} SET answered = true, verdict = {}, answer = {}, amended = {version} \
         WHERE answered != true RETURN id;",
        rid(REQUESTS, id),
        lit(verdict),
        lit(answer)
    )
}

/// Record what a candidate was built against, and edge it to the version so the
/// question "which decision broke the join" is a traversal.
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

/// The rows a statement answered with, or the database's own words.
///
/// `Ok(empty)` for the two shapes that are not failures — a table or a namespace
/// nobody has written yet. The first `current()` of a run always precedes the
/// first `publish`.
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

/// A request id: stable for one `(from, to, subject)` within a version, so a
/// retried ask is one request and a genuinely new ask is a new one.
pub fn request_id(from_part: &str, to_part: &str, subject: &str, at_version: u32) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!("{from_part}|{to_part}|{subject}|{at_version}").as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// One line per disagreement, or nothing at all.
///
/// The message names both sides on purpose: "not composable" without saying which
/// part is on which version sends the reader to the wrong file.
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

/// A row's id with the table and whatever quoting the server chose stripped.
pub fn id_of(row: &Value) -> String {
    row["id"]
        .as_str()
        .and_then(|full| full.split_once(':'))
        .map(|(_, id)| id.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩').to_string())
        .unwrap_or_default()
}
