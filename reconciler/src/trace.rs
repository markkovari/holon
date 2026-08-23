//! What a run leaves behind (ADR-0092).
//!
//! `comp-goalrun` printed to stdout and the run died with the terminal. This
//! appends the run's history to the merged store instead, so "why did branch 3
//! beat branch 7, and what did either of them read" survives the session.
//!
//! ## Why this writes SurrealQL and not `knowledge:memory/observe`
//!
//! `observe` is right there, already appends, already has a transport in
//! `memory.rs`. It is the wrong door: ADR-0084 makes `observe` what a branch
//! BELIEVES and `promote` what the swarm believes. A run's event log is the
//! history those are distilled from, and writing it through `observe` would make
//! "branch 7 started at 12:04" retrievable as advice.
//!
//! Writing directly is also not a violation of "components reach the store
//! through `knowledge:graph`". `goalrun` is not a component — it is the native
//! driver that deploys the fleet and already holds `--surreal-url` as an
//! argument. `comp-capgraph` writes the capability graph's projection the same
//! way, for the same reason (ADR-0091).
//!
//! ## Nothing here may fail a run
//!
//! Every call returns `()`. A run that dies because its telemetry could not be
//! written is strictly worse than a run with no telemetry — the work is the
//! point and the trace is the record of it. Failures are counted and reported
//! once at the end rather than per event, so a database that is down costs one
//! line of output instead of one per attempt.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{json, Value};

/// The tables. `run` and `attempt` are nodes; `event` is the append-only log.
const RUN: &str = "run";
const ATTEMPT: &str = "attempt";
const EVENT: &str = "event";
/// What the pool gained. Its own table because "what can this system do" is a
/// question about the capability, not about the run that happened to add it.
const CAPABILITY: &str = "capability";

/// Where the trace goes, and the credentials to get in.
///
/// `None` everywhere the driver has no `--surreal-url`: a run without a database
/// is a supported configuration (ADR-0080 keeps the database out of the
/// platform), so this is an `Option` the caller holds rather than a hard
/// dependency it must satisfy.
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
    /// A fresh SurrealDB has neither, and every statement against a namespace
    /// that does not exist is rejected — with a 200 carrying a per-statement
    /// error, so it looks like a successful request. `knowledge-graph` defines
    /// them the same way for the same reason.
    ///
    /// Done once, lazily, rather than in `new`: a `Trace` may be constructed for
    /// a run that then writes nothing, and a constructor that dials a database
    /// is a constructor that can hang.
    defined: AtomicBool,
}

impl Trace {
    /// `url` is the SurrealDB HTTP endpoint — the same one the graph component
    /// is pointed at. `password` is the value, already read from its file by the
    /// caller: a path is a secret's location and this takes the secret.
    pub fn new(url: &str, database: &str, password: Option<&str>) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            // The same namespace the graph component uses, so a trace and the
            // lessons it produced are in one place and can be joined (ADR-0091).
            namespace: "comp".to_string(),
            database: database.to_string(),
            user: "root".to_string(),
            // Kept as an Option rather than defaulted. An `--unauthenticated`
            // SurrealDB REJECTS a Basic header naming a user it does not have,
            // so "no password" and "the password is root" are different requests
            // — and `goalrun` already treats absent as "the server takes
            // unauthenticated writes". Defaulting here erased that distinction,
            // and every write against a local unauthenticated database failed.
            password: password.map(|p| p.to_string()),
            dropped: AtomicU64::new(0),
            defined: AtomicBool::new(false),
        }
    }

    /// A run began. Carries what makes the run replayable (ADR-0078): the seed,
    /// and the base every branch is judged against.
    /// `spec` is the goal FILE this run was driven from, relative to the checkout.
    /// It is what joins a run back to the queue entry that started it: `goal` is
    /// prose and two goals can open with the same sentence, so a UI matching on it
    /// hangs a run under whichever entry it happened to find first.
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
        self.event(run, Some(attempt), "branch-spawned", json!({ "branch": branch, "round": round }));
    }

    /// What the gate said (ADR-0088). The score AND the verdict text, because
    /// the verdict is what the next attempt reads and a score alone cannot be
    /// argued with later.
    pub fn gate_verdict(&self, run: &str, attempt: &str, score: u64, passed: bool, verdict: &Value) {
        self.event(
            run,
            Some(attempt),
            "gate-verdict",
            json!({ "score": score, "passed": passed, "verdict": verdict }),
        );
    }

    /// Which lessons reached the prompt. The other half of `attribute`: that
    /// records what happened to what was read, this records what was read.
    pub fn lesson_read(&self, run: &str, attempt: &str, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        self.event(run, Some(attempt), "lesson-read", json!({ "keys": keys }));
    }

    /// An attempt ended, and what it produced.
    ///
    /// `outcome` is `passed`, `failed`, `errored` or `interrupted` — the last one
    /// records a branch that was stopped, whose partial work is discarded rather
    /// than judged (ADR-0092).
    ///
    /// The PATHS are recorded and the contents are not. What a person asks of a
    /// finished run is "which branch touched what, and what did it cost" — and a
    /// branch's whole diff is already in the pull request, addressable and
    /// reviewable, whereas a copy of it here would be a second copy that can
    /// disagree with the first. Cost and duration are the two facts that exist
    /// nowhere else once the terminal is gone.
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
        // NONE rather than 0 when nothing was reported.
        //
        // A finished attempt made at least one model call, so it did not cost zero
        // tokens — a 0 here means the provider reported no usage, which is exactly
        // what `tools/claude-shim.mjs` does on purpose: `claude -p` bills against a
        // subscription and it refuses to fabricate a count. Storing that as 0 makes
        // the wallet read "this run was free" when the truth is "nobody measured",
        // and a column of zeroes is indistinguishable from a cheap run. Absent is
        // the honest answer and the one a reader cannot mistake.
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

    /// A run left the pool able to do something it could not do before.
    ///
    /// This is ADR-0089's whole claim made visible: a solved problem should leave
    /// the system MORE capable, and until now the only way to know it had was to
    /// notice a new directory. A `capability` node rather than only an event,
    /// because "what can this system do, and which run taught it" is a question
    /// about the capability, not about the moment it appeared.
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

    /// Whether reuse discovery found anything. A MISS is the most useful row
    /// here: it is the graph naming a capability the pool lacks, which is the
    /// signal for what to build next (ADR-0089).
    pub fn capsearch(&self, run: &str, query: &str, hits: usize) {
        let kind = if hits == 0 { "capsearch-miss" } else { "capsearch-hit" };
        self.event(run, None, kind, json!({ "query": query, "hits": hits }));
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
        self.event(run, None, "run-resolved", json!({ "outcome": outcome, "winner": winner, "url": url }));
    }

    /// One line for the operator, or nothing when everything landed.
    ///
    /// Reported once rather than per failure: a database that is down should
    /// cost one line, not one per attempt drowning the run's real output.
    pub fn report(&self) -> Option<String> {
        match self.dropped.load(Ordering::Relaxed) {
            0 => None,
            n => Some(format!(
                "{n} trace write(s) did not land — the run is unaffected, its history is incomplete"
            )),
        }
    }

    // ---- the append ---------------------------------------------------------

    fn event(&self, run: &str, attempt: Option<&str>, kind: &str, data: Value) {
        // No id: an event is append-only and never addressed individually, so
        // letting SurrealDB mint one avoids inventing a key that has to be
        // unique across concurrent branches.
        self.send(&format!(
            "CREATE {EVENT} SET run = {}, attempt = {}, kind = {}, data = {}, at = time::now();",
            lit(run),
            lit(attempt.unwrap_or_default()),
            lit(kind),
            data,
        ));
    }

    /// Define the namespace and database, once.
    ///
    /// Marked done even when the call fails: retrying it before every write on a
    /// database that is down turns one dropped write into two, and the write
    /// that follows will fail and be counted anyway.
    fn ensure_defined(&self) {
        if self.defined.swap(true, Ordering::Relaxed) {
            return;
        }
        let surql = format!(
            "DEFINE NAMESPACE IF NOT EXISTS {}; USE NS {}; DEFINE DATABASE IF NOT EXISTS {};",
            self.namespace, self.namespace, self.database
        );
        let _ = ureq_post(&self.url, &self.namespace, &self.database, &self.user, self.password.as_deref(), &surql);
    }

    fn send(&self, surql: &str) {
        self.ensure_defined();
        let ok = ureq_post(&self.url, &self.namespace, &self.database, &self.user, self.password.as_deref(), surql);
        if !ok {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// POST one body to `/sql`. Returns whether every statement in it was accepted.
fn ureq_post(url: &str, ns: &str, db: &str, user: &str, password: Option<&str>, surql: &str) -> bool {
    let client = match reqwest::blocking::Client::builder()
        // Short on purpose: a trace write must never be the reason a run stalls,
        // and the run has already done the expensive part by the time we write.
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut req = client.post(format!("{url}/sql"));
    // Only when there is one: see the `password` field.
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
    // A 200 whose statements were rejected is the failure that would otherwise
    // look like a success — SurrealDB answers 200 with per-statement status.
    let text = r.text().unwrap_or_default();
    serde_json::from_str::<Vec<Value>>(&text)
        .map(|statements| statements.iter().all(|s| s["status"] == "OK"))
        .unwrap_or(false)
}

/// `tokens` as SurrealQL: the count, or `NONE` when nothing was reported.
///
/// See `attempt_finished` for why absent and zero must not be the same value.
fn tokens_sql(tokens: u64) -> String {
    if tokens == 0 {
        "NONE".to_string()
    } else {
        tokens.to_string()
    }
}

/// The paths a branch wrote, from the driver's `[{path, content}]`.
///
/// Sorted and de-duplicated: a branch that rewrites the same file across repairs
/// reports it once, and a stable order means two runs of the same branch produce
/// comparable rows rather than a diff that is only ordering.
fn file_paths(files: &Value) -> Vec<String> {
    let mut out: Vec<String> = files
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["path"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// A record id. `⟨…⟩` is SurrealDB's own quoting for an arbitrary id, and the
/// closing bracket is the one character that could end it early. Run ids carry
/// `/` (`seed/g1/risk-first`), which is exactly why they need quoting.
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

    /// A goal is prose a person wrote, and prose contains quotes. If it reached
    /// the statement unescaped it would either break the write or — worse —
    /// close the string and run as SurrealQL.
    #[test]
    fn a_goal_containing_quotes_cannot_break_out_of_its_statement() {
        let nasty = r#"add a "search" box'; DROP TABLE memory; --"#;
        let l = lit(nasty);
        assert!(l.starts_with('"') && l.ends_with('"'));
        assert!(!l[1..l.len() - 1].contains('"') || l.contains("\\\""), "inner quotes must be escaped: {l}");
        assert!(
            serde_json::from_str::<String>(&l).unwrap() == nasty,
            "the literal must round-trip to exactly what was passed"
        );
    }

    /// Run ids are `seed/g1/branch` — the `/` is why the id is bracket-quoted at
    /// all, and a `⟩` in one would end the quoting early.
    #[test]
    fn a_run_id_keeps_its_slashes_and_cannot_end_its_own_quoting() {
        assert_eq!(rid("run", "42/g1/risk-first"), "run:⟨42/g1/risk-first⟩");
        assert_eq!(rid("run", "42⟩; DELETE memory"), "run:⟨42; DELETE memory⟩");
    }

    /// Dropped writes are counted, and silence means everything landed — the
    /// report is what tells an operator the history is incomplete.
    #[test]
    fn an_unmeasured_cost_is_absent_rather_than_zero() {
        // The shim reports no usage on purpose — `claude -p` bills against a
        // subscription and it will not fabricate a count. Stored as 0, a whole run
        // reads as free; stored as NONE, it reads as unmeasured, which is true.
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
