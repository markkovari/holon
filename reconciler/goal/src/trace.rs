//! A run's history in the store (ADR-0092). Direct SurrealQL, not `observe`, which holds beliefs.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{json, Value};

/// `run` and `attempt` are nodes; `event` is the append-only log.
const RUN: &str = "run";
const ATTEMPT: &str = "attempt";
const EVENT: &str = "event";
/// Its own table: "what can this system do" is about the capability, not the run.
const CAPABILITY: &str = "capability";

/// Optional to the caller: a run without a database is supported (ADR-0080).
pub struct Trace {
    url: String,
    namespace: String,
    database: String,
    user: String,
    /// `None` means send NO auth header at all.
    password: Option<String>,
    /// Reported once, at the end.
    dropped: AtomicU64,
    /// Defined lazily, so constructing a `Trace` never dials a database.
    defined: AtomicBool,
}

impl Trace {
    /// `password` is the value, already read from its file.
    pub fn new(url: &str, database: &str, password: Option<&str>) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            // The graph component's namespace, so a trace and its lessons can be joined (ADR-0091).
            namespace: "comp".to_string(),
            database: database.to_string(),
            user: "root".to_string(),
            // An unauthenticated SurrealDB rejects a Basic header naming a user it does not have.
            password: password.map(|p| p.to_string()),
            dropped: AtomicU64::new(0),
            defined: AtomicBool::new(false),
        }
    }

    /// Carries the seed and base for replay (ADR-0078); `spec` joins a run to its queue entry.
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

    /// Score and verdict text, since the next attempt reads the verdict (ADR-0088).
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

    /// The other half of `attribute`.
    pub fn lesson_read(&self, run: &str, attempt: &str, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        self.event(run, Some(attempt), "lesson-read", json!({ "keys": keys }));
    }

    /// Paths, not contents: the diff is in the PR and a copy could disagree.
    // Each is its own recorded fact, not fields to extract.
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
        // NONE, not 0: the provider reported no usage (the Claude shim on purpose), not "free".
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

    /// A node, not only an event: "which run taught it" is about the capability (ADR-0089).
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

    /// A miss is the most useful row: the graph naming a capability the pool lacks (ADR-0089).
    pub fn capsearch(&self, run: &str, query: &str, hits: usize) {
        let kind = if hits == 0 { "capsearch-miss" } else { "capsearch-hit" };
        self.event(run, None, kind, json!({ "query": query, "hits": hits }));
    }

    /// Only fires with a reachable advisor, so absence means "not asked".
    pub fn capsearch_confirmed(&self, run: &str, confirmed: usize, rejected: usize) {
        self.event(
            run,
            None,
            "capsearch-confirmed",
            json!({ "confirmed": confirmed, "rejected": rejected }),
        );
    }

    /// `merged`, `failed`, `exhausted` or `interrupted`.
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

    /// Once, not per failure; `None` when everything landed.
    pub fn report(&self) -> Option<String> {
        match self.dropped.load(Ordering::Relaxed) {
            0 => None,
            n => Some(format!(
                "{n} trace write(s) did not land — the run is unaffected, its history is incomplete"
            )),
        }
    }

    fn event(&self, run: &str, attempt: Option<&str>, kind: &str, data: Value) {
        // No id: SurrealDB mints one unique across concurrent branches.
        self.send(&format!(
            "CREATE {EVENT} SET run = {}, attempt = {}, kind = {}, data = {}, at = time::now();",
            lit(run),
            lit(attempt.unwrap_or_default()),
            lit(kind),
            data,
        ));
    }

    /// Marked done even on failure: retrying on a down database would double the dropped writes.
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
    // A 200 can carry per-statement rejections.
    let text = r.text().unwrap_or_default();
    serde_json::from_str::<Vec<Value>>(&text)
        .map(|statements| statements.iter().all(|s| s["status"] == "OK"))
        .unwrap_or(false)
}

fn tokens_sql(tokens: u64) -> String {
    if tokens == 0 {
        "NONE".to_string()
    } else {
        tokens.to_string()
    }
}

/// Sorted and de-duplicated so repeated runs compare without ordering noise.
fn file_paths(files: &Value) -> Vec<String> {
    let mut out: Vec<String> = files
        .as_array()
        .map(|a| a.iter().filter_map(|f| f["path"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// `⟨…⟩` quotes ids carrying `/`; `⟩` is the one character that could end it.
fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

/// Through JSON, so a value cannot carry syntax (ADR-0080).
fn lit(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_run_id_keeps_its_slashes_and_cannot_end_its_own_quoting() {
        assert_eq!(rid("run", "42/g1/risk-first"), "run:⟨42/g1/risk-first⟩");
        assert_eq!(rid("run", "42⟩; DELETE memory"), "run:⟨42; DELETE memory⟩");
    }

    #[test]
    fn an_unmeasured_cost_is_absent_rather_than_zero() {
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
