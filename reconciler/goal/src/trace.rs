//! What a run leaves behind (ADR-0092): the run's history appended to the merged store, so "why did
//! branch 3 beat branch 7" survives the terminal.
//!
//! Writes SurrealQL directly, not `knowledge:memory/observe`: ADR-0084 makes `observe` what a
//! branch BELIEVES, and an event log written through it would make "branch 7 started at 12:04"
//! retrievable as advice. `goalrun` is a native driver, not a component, so this does not bypass
//! `knowledge:graph` (`comp-capgraph` does the same, ADR-0091).
//!
//! Nothing here may fail a run: every call returns `()`. Failures are counted and reported once at
//! the end, so a down database costs one line, not one per event.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{json, Value};

/// The tables. `run` and `attempt` are nodes; `event` is the append-only log.
const RUN: &str = "run";
const ATTEMPT: &str = "attempt";
const EVENT: &str = "event";
/// What the pool gained. Its own table: "what can this system do" is about the capability, not the
/// run that added it.
const CAPABILITY: &str = "capability";

/// Where the trace goes, and the credentials to get in. An `Option` the caller holds: a run
/// without a database is supported (ADR-0080).
pub struct Trace {
    url: String,
    namespace: String,
    database: String,
    user: String,
    /// `None` means send NO auth header at all.
    password: Option<String>,
    /// Writes that did not land. Reported once, at the end.
    dropped: AtomicU64,
    /// Whether the namespace and database have been defined this process.
    ///
    /// A fresh SurrealDB has neither, and statements against a missing namespace come back as a 200
    /// carrying a per-statement error. Done once, lazily, not in `new`, so constructing a `Trace`
    /// never dials a database.
    defined: AtomicBool,
}

impl Trace {
    /// `url` is the SurrealDB HTTP endpoint. `password` is the value, already read from its file.
    pub fn new(url: &str, database: &str, password: Option<&str>) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            // The graph component's namespace, so a trace and its lessons can be joined (ADR-0091).
            namespace: "comp".to_string(),
            database: database.to_string(),
            user: "root".to_string(),
            // An Option, not defaulted: an `--unauthenticated` SurrealDB REJECTS a Basic header
            // naming a user it does not have, so "no password" differs from "the password is root".
            password: password.map(|p| p.to_string()),
            dropped: AtomicU64::new(0),
            defined: AtomicBool::new(false),
        }
    }

    /// A run began. Carries what makes it replayable (ADR-0078): the seed and the base every branch
    /// is judged against. `spec` is the goal FILE, relative to the checkout; it joins a run to its
    /// queue entry, since two goals can open with the same sentence.
    pub fn run_started(
        &self,
        run: &str,
        goal: &str,
        spec: &str,
        seed: u64,
        base_commit: &str,
        branches: u32,
    ) {
        self.send(&format!(
            "UPSERT {} SET id_text = {}, goal = {}, spec = {}, seed = {seed}, base_commit = {}, \
             branches = {branches}, started_at = time::now(), resolved_at = NONE;",
            rid(RUN, run),
            lit(run),
            lit(goal),
            lit(spec),
            lit(base_commit),
        ));
        self.event(run, None, "run-started", json!({ "goal": goal, "seed": seed }));
    }

    /// A branch of a generation was spawned.
    pub fn branch_spawned(&self, run: &str, attempt: &str, branch: &str, round: usize) {
        self.send(&format!(
            "UPSERT {} SET id_text = {}, run = {}, branch = {}, round = {round}, \
             started_at = time::now();",
            rid(ATTEMPT, attempt),
            lit(attempt),
            lit(run),
            lit(branch),
        ));
        self.event(
            run,
            Some(attempt),
            "branch-spawned",
            json!({ "branch": branch, "round": round }),
        );
    }

    /// What the gate said (ADR-0088): the score AND the verdict text, since the next attempt reads
    /// the verdict.
    pub fn gate_verdict(
        &self,
        run: &str,
        attempt: &str,
        score: u64,
        passed: bool,
        verdict: &Value,
    ) {
        self.event(
            run,
            Some(attempt),
            "gate-verdict",
            json!({ "score": score, "passed": passed, "verdict": verdict }),
        );
    }

    /// Which lessons reached the prompt. The other half of `attribute`.
    pub fn lesson_read(&self, run: &str, attempt: &str, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        self.event(run, Some(attempt), "lesson-read", json!({ "keys": keys }));
    }

    /// An attempt ended, and what it produced.
    ///
    /// `outcome` is `passed`, `failed`, `errored` or `interrupted` (a stopped branch whose partial
    /// work is discarded, ADR-0092).
    ///
    /// PATHS are recorded, not contents: the diff is already in the pull request and a copy here
    /// could disagree. Cost and duration exist nowhere else once the terminal is gone.
    // Two over clippy's default threshold; each is its own recorded fact, not fields to extract.
    #[allow(clippy::too_many_arguments)]
    pub fn attempt_finished(
        &self,
        run: &str,
        attempt: &str,
        outcome: &str,
        score: u64,
        files: &Value,
        tokens: u64,
        elapsed_ms: u64,
        tries: u64,
    ) {
        let paths = file_paths(files);
        // NONE rather than 0 when nothing was reported: a finished attempt made model calls, so 0
        // means the provider reported no usage (`tools/claude-shim.mjs` does this on purpose).
        // Storing 0 would make the wallet read "free" when the truth is "nobody measured".
        let tokens_sql = tokens_sql(tokens);
        self.send(&format!(
            "UPSERT {} SET outcome = {}, score = {score}, paths = {}, files = {}, \
             tokens = {tokens_sql}, elapsed_ms = {elapsed_ms}, tries = {tries}, \
             resolved_at = time::now();",
            rid(ATTEMPT, attempt),
            lit(outcome),
            json!(paths),
            paths.len(),
        ));
        self.event(
            run,
            Some(attempt),
            "attempt-finished",
            json!({ "outcome": outcome, "score": score, "files": paths.len(),
                    "tokens": tokens, "tries": tries }),
        );
    }

    /// A run left the pool able to do something it could not before (ADR-0089). A `capability` node
    /// rather than only an event, since "what can this system do, and which run taught it" is about
    /// the capability.
    pub fn capability_added(&self, run: &str, name: &str, path: &str) {
        self.send(&format!(
            "UPSERT {} SET name = {}, path = {}, added_by = {}, added_at = time::now();",
            rid(CAPABILITY, name),
            lit(name),
            lit(path),
            lit(run),
        ));
        self.event(run, None, "capability-added", json!({ "name": name, "path": path }));
    }

    /// Whether reuse discovery found anything. A MISS is the most useful row: the graph naming a
    /// capability the pool lacks (ADR-0089).
    pub fn capsearch(&self, run: &str, query: &str, hits: usize) {
        let kind = if hits == 0 { "capsearch-miss" } else { "capsearch-hit" };
        self.event(run, None, kind, json!({ "query": query, "hits": hits }));
    }

    /// How many of `capsearch`'s hits `capability-advisor` confirmed actually fit the goal. Only
    /// fires when an advisor was configured AND reachable, so absence means "not asked".
    pub fn capsearch_confirmed(&self, run: &str, confirmed: usize, rejected: usize) {
        self.event(
            run,
            None,
            "capsearch-confirmed",
            json!({ "confirmed": confirmed, "rejected": rejected }),
        );
    }

    /// How the run ended: `merged`, `failed`, `exhausted`, `interrupted`.
    pub fn run_resolved(&self, run: &str, outcome: &str, winner: Option<&str>, url: &str) {
        self.send(&format!(
            "UPSERT {} SET outcome = {}, winner = {}, url = {}, resolved_at = time::now();",
            rid(RUN, run),
            lit(outcome),
            lit(winner.unwrap_or_default()),
            lit(url),
        ));
        self.event(
            run,
            None,
            "run-resolved",
            json!({ "outcome": outcome, "winner": winner, "url": url }),
        );
    }

    /// One line for the operator, or nothing when everything landed. Once, not per failure.
    pub fn report(&self) -> Option<String> {
        match self.dropped.load(Ordering::Relaxed) {
            0 => None,
            n => Some(format!(
                "{n} trace write(s) did not land — the run is unaffected, its history is incomplete"
            )),
        }
    }

    fn event(&self, run: &str, attempt: Option<&str>, kind: &str, data: Value) {
        // No id: an event is append-only, so SurrealDB mints one rather than us inventing a key
        // unique across concurrent branches.
        self.send(&format!(
            "CREATE {EVENT} SET run = {}, attempt = {}, kind = {}, data = {}, at = time::now();",
            lit(run),
            lit(attempt.unwrap_or_default()),
            lit(kind),
            data,
        ));
    }

    /// Define the namespace and database, once. Marked done even when the call fails: retrying
    /// before every write on a down database would just double the dropped writes.
    fn ensure_defined(&self) {
        if self.defined.swap(true, Ordering::Relaxed) {
            return;
        }
        let surql = format!(
            "DEFINE NAMESPACE IF NOT EXISTS {}; USE NS {}; DEFINE DATABASE IF NOT EXISTS {};",
            self.namespace, self.namespace, self.database
        );
        let _ = ureq_post(
            &self.url,
            &self.namespace,
            &self.database,
            &self.user,
            self.password.as_deref(),
            &surql,
        );
    }

    fn send(&self, surql: &str) {
        self.ensure_defined();
        let ok = ureq_post(
            &self.url,
            &self.namespace,
            &self.database,
            &self.user,
            self.password.as_deref(),
            surql,
        );
        if !ok {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn ureq_post(
    url: &str,
    ns: &str,
    db: &str,
    user: &str,
    password: Option<&str>,
    surql: &str,
) -> bool {
    let client = match reqwest::blocking::Client::builder()
        // Short on purpose: a trace write must never be why a run stalls.
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut req = client.post(format!("{url}/sql"));
    if let Some(p) = password {
        req = req.basic_auth(user, Some(p));
    }
    let Ok(r) = req
        .header("accept", "application/json")
        .header("surreal-ns", ns)
        .header("surreal-db", db)
        .body(surql.to_string())
        .send()
    else {
        return false;
    };
    if !r.status().is_success() {
        return false;
    }
    // A 200 can carry per-statement rejections, so check each status.
    let text = r.text().unwrap_or_default();
    serde_json::from_str::<Vec<Value>>(&text)
        .map(|statements| statements.iter().all(|s| s["status"] == "OK"))
        .unwrap_or(false)
}

/// `tokens` as SurrealQL: the count, or `NONE` when nothing was reported (see `attempt_finished`).
fn tokens_sql(tokens: u64) -> String {
    if tokens == 0 {
        "NONE".to_string()
    } else {
        tokens.to_string()
    }
}

/// The paths a branch wrote, from the driver's `[{path, content}]`. Sorted and de-duplicated so
/// rows from repeated runs compare without ordering noise.
fn file_paths(files: &Value) -> Vec<String> {
    let mut out: Vec<String> = files
        .as_array()
        .map(|a| a.iter().filter_map(|f| f["path"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// A record id. `⟨…⟩` is SurrealDB's quoting for an arbitrary id; run ids carry `/`
/// (`seed/g1/risk-first`), which is why they need it, and `⟩` is the one character that could end it.
fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

/// A string literal, through JSON so a value cannot carry syntax (ADR-0080).
fn lit(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A goal is prose and contains quotes; unescaped it would break the write or close the string
    /// and run as SurrealQL.
    #[test]
    fn a_goal_containing_quotes_cannot_break_out_of_its_statement() {
        let nasty = r#"add a "search" box'; DROP TABLE memory; --"#;
        let l = lit(nasty);
        assert!(l.starts_with('"') && l.ends_with('"'));
        assert!(
            !l[1..l.len() - 1].contains('"') || l.contains("\\\""),
            "inner quotes must be escaped: {l}"
        );
        assert!(
            serde_json::from_str::<String>(&l).unwrap() == nasty,
            "the literal must round-trip to exactly what was passed"
        );
    }

    /// Run ids carry `/`, which is why the id is bracket-quoted, and a `⟩` would end it early.
    #[test]
    fn a_run_id_keeps_its_slashes_and_cannot_end_its_own_quoting() {
        assert_eq!(rid("run", "42/g1/risk-first"), "run:⟨42/g1/risk-first⟩");
        assert_eq!(rid("run", "42⟩; DELETE memory"), "run:⟨42; DELETE memory⟩");
    }

    /// Dropped writes are counted; silence means everything landed.
    #[test]
    fn an_unmeasured_cost_is_absent_rather_than_zero() {
        // The shim reports no usage on purpose. Stored as 0 a run reads as free; as NONE, unmeasured.
        assert_eq!(tokens_sql(0), "NONE");
        assert_eq!(tokens_sql(1), "1");
        assert_eq!(tokens_sql(31_500), "31500");
    }

    #[test]
    fn nothing_is_reported_until_something_is_dropped() {
        let t = Trace::new("http://127.0.0.1:1", "goalmemory", None);
        assert!(t.report().is_none());
        t.dropped.fetch_add(2, Ordering::Relaxed);
        assert!(t.report().unwrap().contains("2 trace write(s)"));
    }
}
