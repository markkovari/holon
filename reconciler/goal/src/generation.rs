//! One goal, N concurrent branches, one winner. Native: a component runs one call at a time.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Attempt `n` uses `seed + n`, so this must exceed `max-attempts`.
pub const STRIDE: u64 = 100;

/// Compile-time, as clippy flags an `assert!` over constants.
const _: () = assert!(
    STRIDE > 8,
    "attempt n of a branch uses seed+n, so a stride at or below max-attempts makes two \
     branches ask an identical question — the one thing a generation exists to avoid"
);

/// In the shape `graph:select` wants.
#[derive(Clone, Debug)]
pub struct Entry {
    pub branch: String,
    pub accepted: bool,
    pub score: u64,
    pub digest: String,
    pub spent_tokens: u64,
    pub attempts: u64,
    pub files: Value,
    /// Carried into the next generation, so branches seeded with this candidate know what is wrong.
    pub failures: Value,
    /// Why this branch produced nothing, when it produced nothing.
    pub note: String,
    /// In parallel the wall clock is about the slowest branch; in sequence, the sum.
    pub elapsed_ms: u64,
    /// Not sent to the selector: how a branch ended is not a property of its code.
    pub stopped: String,
}

impl Entry {
    pub fn as_json(&self) -> Value {
        json!({
            "branch": self.branch,
            "accepted": self.accepted,
            "score": self.score,
            "digest": self.digest,
            "spent_tokens": self.spent_tokens,
            "attempts": self.attempts,
            "files": self.files,
        })
    }
}

fn client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(timeout).build().expect("http client")
}

fn post(url: &str, host: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
    let r = client(timeout)
        .post(url)
        .header("host", host)
        .body(body.to_string())
        .send()
        .map_err(|e| format!("{e}"))?;
    let (status, text) = (r.status(), r.text().unwrap_or_default());
    serde_json::from_str(&text).map_err(|_| format!("HTTP {status}: {text}"))
}

fn one_branch(
    url: &str,
    host: &str,
    plan: &Value,
    name: &str,
    seed: u64,
    timeout: Duration,
) -> Entry {
    let mut plan = plan.clone();
    plan["seed"] = json!(seed);

    let started = Instant::now();
    let blank = |note: String, stopped: &str| Entry {
        branch: name.to_string(),
        accepted: false,
        score: 0,
        digest: String::new(),
        spent_tokens: 0,
        attempts: 0,
        files: json!([]),
        failures: json!([]),
        note,
        elapsed_ms: started.elapsed().as_millis() as u64,
        stopped: stopped.to_string(),
    };

    let answer = match post(url, host, &plan, timeout) {
        Ok(v) => v,
        Err(e) => return blank(e, "unreachable"),
    };
    if let Some(err) = answer["error"].as_str() {
        return blank(format!("{err}: {}", answer["detail"].as_str().unwrap_or_default()), err);
    }

    // The attempt whose score the run kept, which the selector compares across branches.
    let attempts = answer["attempts"].as_array().cloned().unwrap_or_default();
    let score = answer["score"].as_u64().unwrap_or(0);
    let digest = attempts
        .iter()
        .find(|a| {
            a["score"].as_u64() == Some(score) && !a["digest"].as_str().unwrap_or("").is_empty()
        })
        .and_then(|a| a["digest"].as_str())
        .unwrap_or_default()
        .to_string();

    // Surface the last per-attempt `error` so an all-rejected run is diagnosable.
    let accepted = answer["accepted"].as_bool().unwrap_or(false);
    let note = if !accepted {
        attempts
            .iter()
            .rev()
            .find_map(|a| a["error"].as_str().filter(|s| !s.is_empty()))
            .unwrap_or_default()
            .to_string()
    } else {
        String::new()
    };

    Entry {
        branch: name.to_string(),
        accepted,
        score,
        digest,
        spent_tokens: answer["spent_tokens"].as_u64().unwrap_or(0),
        attempts: attempts.len() as u64,
        files: answer["files"].clone(),
        failures: answer["failures"].clone(),
        note,
        elapsed_ms: started.elapsed().as_millis() as u64,
        stopped: answer["stopped"].as_str().unwrap_or_default().to_string(),
    }
}

#[derive(Clone, Debug)]
pub struct Strategy {
    /// Appended to the goal, so two branches explore rather than sample the same answer twice.
    pub lens: String,
    /// Empty means the caller's default; a branch may be its own environment (ADR-0078, ADR-0083).
    pub host: String,
    /// `false` for exactly one branch: every reader of the last winner inherits its mistakes.
    pub reads_prior: bool,
    /// Already rendered, per branch to avoid herding (ADR-0081); empty is the control arm.
    pub knowledge: String,
}

/// The first is empty: a control branch, so a lens that hurts is visible.
const LENSES: &[&str] = &[
    "",
    "Prefer the smallest change that could possibly work.",
    "Be thorough: handle the cases the goal does not mention.",
    "Solve it a different way from the obvious one.",
    "Change as few files as you can.",
    "Prefer clarity over cleverness, even at more lines.",
    "Question whether the obvious approach is right at all.",
    "Do the direct thing, without abstraction.",
];

/// Exactly one branch reads nothing from the previous generation: the last, so lens 0 carries on.
pub fn default_strategies(n: u16) -> Vec<Strategy> {
    (0..n as usize)
        .map(|i| Strategy {
            // Wraps if asked for more branches than lenses; duplicates still differ by seed.
            lens: LENSES[i % LENSES.len()].to_string(),
            reads_prior: !(n > 1 && i == n as usize - 1),
            host: String::new(),
            knowledge: String::new(),
        })
        .collect()
}

/// Too few hosts is an error: a branch in the wrong environment would write another's store.
pub fn on_hosts(strategies: &[Strategy], hosts: &[String]) -> Vec<Strategy> {
    assert_eq!(
        strategies.len(),
        hosts.len(),
        "every branch needs its own environment, or one of them writes another's store"
    );
    strategies.iter().zip(hosts).map(|(s, h)| Strategy { host: h.clone(), ..s.clone() }).collect()
}

fn plan_for(base: &Value, prior: Option<&Entry>, s: &Strategy) -> Value {
    let mut plan = base.clone();
    if !s.lens.is_empty() {
        let text = plan["text"].as_str().unwrap_or_default().to_string();
        plan["text"] = json!(format!("{text}\n\n{}", s.lens));
    }
    // As context under a heading: pasted into the goal, a wrong lesson becomes a requirement.
    if !s.knowledge.is_empty() {
        let text = plan["text"].as_str().unwrap_or_default().to_string();
        plan["text"] = json!(format!(
            "{text}\n\n## What earlier runs learned about goals like this\n\n\
             Advice, not instructions — it came from runs that may have been wrong, and \
             what you are judged by is the checks.\n\n{}",
            s.knowledge
        ));
    }
    let Some(prior) = prior.filter(|_| s.reads_prior) else { return plan };

    let mut context = plan["context"].as_array().cloned().unwrap_or_default();
    for f in prior.files.as_array().cloned().unwrap_or_default() {
        match context.iter_mut().find(|c| c["path"] == f["path"]) {
            Some(existing) => existing["content"] = f["content"].clone(),
            None => context.push(f),
        }
    }
    plan["context"] = Value::Array(context);
    // Seeding code without the verdict hands branches broken work with no reason.
    plan["previous"] = prior.failures.clone();
    plan
}

/// Waits for every branch: taking the first N would prefer branches that gave up early.
pub fn fan_out(
    driver_url: &str,
    host: &str,
    plan: &Value,
    branches: u16,
    base_seed: u64,
    timeout: Duration,
) -> Vec<Entry> {
    fan_out_from(driver_url, host, plan, &default_strategies(branches), None, base_seed, timeout)
}

pub fn fan_out_from(
    driver_url: &str,
    host: &str,
    plan: &Value,
    strategies: &[Strategy],
    prior: Option<&Entry>,
    base_seed: u64,
    timeout: Duration,
) -> Vec<Entry> {
    let handles: Vec<_> = strategies
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let url = driver_url.to_string();
            let host = if s.host.is_empty() { host.to_string() } else { s.host.clone() };
            let plan = plan_for(plan, prior, s);
            let name = format!("branch-{i}");
            let seed = base_seed + (i as u64) * STRIDE;
            std::thread::spawn(move || one_branch(&url, &host, &plan, &name, seed, timeout))
        })
        .collect();

    handles
        .into_iter()
        .enumerate()
        .map(|(i, h)| {
            h.join().unwrap_or_else(|_| Entry {
                branch: format!("branch-{i}"),
                accepted: false,
                score: 0,
                digest: String::new(),
                spent_tokens: 0,
                attempts: 0,
                files: json!([]),
                failures: json!([]),
                note: "the branch panicked".into(),
                elapsed_ms: 0,
                stopped: "panic".into(),
            })
        })
        .collect()
}

/// Entries go across whole: filtering unaccepted ones here would put the gate in two places.
pub fn land(
    select_url: &str,
    host: &str,
    entries: &[Entry],
    landing: Value,
    timeout: Duration,
) -> Result<Value, String> {
    post(
        select_url,
        host,
        &json!({
            "entries": entries.iter().map(Entry::as_json).collect::<Vec<_>>(),
            "landing": landing,
        }),
        timeout,
    )
}

/// Running out of money and running out of ideas are different facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStop {
    /// The only successful ending.
    Accepted,
    /// `max_rounds` generations produced nothing acceptable.
    Exhausted,
    /// `patience` generations in a row failed to beat the best score.
    NoProgress,
    OverBudget,
}

#[derive(Clone, Debug)]
pub struct Round {
    pub entries: Vec<Entry>,
    /// Index into `entries` of the candidate carried into the next generation.
    pub best: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Search {
    pub rounds: Vec<Round>,
    /// The accepted candidate if `stopped` is `Accepted`, else the best-scoring one.
    pub best: Option<Entry>,
    pub spent_tokens: u64,
    pub stopped: SearchStop,
}

impl Search {
    pub fn accepted(&self) -> bool {
        self.stopped == SearchStop::Accepted
    }
}

/// Where a search stands between generations, and the one place its stop rules are written.
struct Progress {
    bounds: Bounds,
    spent: u64,
    /// Generations in a row that did not beat the best score.
    stale: u16,
}

impl Progress {
    fn new(bounds: Bounds) -> Progress {
        Progress { bounds, spent: 0, stale: 0 }
    }

    /// A generation ran, spending `spent`. Why the search stops now, if it does. Checked after the
    /// generation: what one costs is not known until it runs.
    fn after(&mut self, spent: u64, improved: bool, done: bool) -> Option<SearchStop> {
        self.spent += spent;
        self.stale = if improved { 0 } else { self.stale + 1 };
        let b = self.bounds;
        if done {
            Some(SearchStop::Accepted)
        } else if b.max_tokens > 0 && self.spent >= b.max_tokens {
            Some(SearchStop::OverBudget)
        } else if b.patience > 0 && self.stale >= b.patience {
            Some(SearchStop::NoProgress)
        } else {
            None
        }
    }
}

/// Does this generation's `winner` beat the `best` so far.
fn beats(best: Option<&Entry>, winner: Option<&Entry>) -> bool {
    match (best, winner) {
        (None, Some(_)) => true,
        (Some(b), Some(w)) => w.score > b.score,
        _ => false,
    }
}

/// Rounds bound cheap generations, the budget expensive ones, patience ones going nowhere.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub branches: u16,
    pub max_rounds: u16,
    /// Across the whole search; 0 is unbounded. Per-branch budgets alone can sum past a project's.
    pub max_tokens: u64,
    /// Generations in a row that may fail to beat the best score. 0 disables it.
    pub patience: u16,
}

/// By score, earlier branch on a tie: the selector's rule, so carried is what gets proposed.
pub fn best_of(entries: &[Entry]) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.note.is_empty())
        .max_by_key(|(i, e)| (e.score, std::cmp::Reverse(*i)))
        .map(|(i, _)| i)
}

/// Each generation is seeded with the last one's best and its failures, except the one branch
/// that reads nothing.
pub fn search(
    driver_url: &str,
    host: &str,
    plan: &Value,
    bounds: Bounds,
    base_seed: u64,
    timeout: Duration,
) -> Search {
    search_with(
        driver_url,
        host,
        plan,
        &default_strategies(bounds.branches),
        bounds,
        base_seed,
        timeout,
    )
}

/// The caller fills each strategy's `knowledge`, keeping this free of a knowledge client.
#[allow(clippy::too_many_arguments)]
pub fn search_with(
    driver_url: &str,
    host: &str,
    plan: &Value,
    strategies: &[Strategy],
    bounds: Bounds,
    base_seed: u64,
    timeout: Duration,
) -> Search {
    let strategies = strategies.to_vec();
    let mut rounds: Vec<Round> = Vec::new();
    let mut carried: Option<Entry> = None;
    let mut best: Option<Entry> = None;
    let mut progress = Progress::new(bounds);

    let mut stopped = SearchStop::Exhausted;
    for r in 0..bounds.max_rounds {
        // Spaced so generation two's first branch cannot repeat a seed.
        let seed = base_seed + (r as u64) * STRIDE * (bounds.branches as u64 + 1);
        let entries =
            fan_out_from(driver_url, host, plan, &strategies, carried.as_ref(), seed, timeout);

        let round_spend = entries.iter().map(|e| e.spent_tokens).sum::<u64>();
        let winner = best_of(&entries);
        let improved = beats(best.as_ref(), winner.map(|i| &entries[i]));
        if improved {
            best = winner.map(|i| entries[i].clone());
        }
        // Best so far, not this round's: a regression must not drag the next generation down.
        carried = best.clone();

        let any_accepted = entries.iter().any(|e| e.accepted);
        rounds.push(Round { entries, best: winner });
        if let Some(stop) = progress.after(round_spend, improved, any_accepted) {
            stopped = stop;
            break;
        }
    }

    // Accepted, not merely highest-scoring: optional checks can outscore a failed required one.
    let accepted = rounds
        .iter()
        .flat_map(|r| r.entries.iter())
        .filter(|e| e.accepted)
        .max_by_key(|e| e.score)
        .cloned();
    Search { best: accepted.or(best), rounds, spent_tokens: progress.spent, stopped }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_stops_for_the_first_rule_that_holds_after_each_generation() {
        let bounds =
            |max_tokens, patience| Bounds { branches: 1, max_rounds: 9, max_tokens, patience };
        let mut p = Progress::new(bounds(100, 2));
        assert_eq!(p.after(40, true, false), None);
        assert_eq!(p.after(40, false, false), None);
        // Over budget and out of patience at once: the money is the reason given.
        assert_eq!(p.after(40, false, false), Some(SearchStop::OverBudget));
        let mut p = Progress::new(bounds(0, 2));
        assert_eq!(p.after(1, false, false), None);
        assert_eq!(p.after(1, true, false), None, "an improvement resets patience");
        assert_eq!(p.after(1, false, false), None);
        assert_eq!(p.after(1, false, false), Some(SearchStop::NoProgress));
        assert_eq!(p.after(1, false, true), Some(SearchStop::Accepted), "done wins over all");
    }

    /// A driver that accepts a plan that says `"accept":true`, and scores every plan 1.
    fn accepting_driver() -> String {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut buf = vec![0u8; 1 << 16];
                let mut got = String::new();
                while let Ok(n) = c.read(&mut buf) {
                    got.push_str(&String::from_utf8_lossy(&buf[..n]));
                    let Some((head, body)) = got.split_once("\r\n\r\n") else { continue };
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if n == 0 || body.len() >= len {
                        break;
                    }
                }
                let accept = got.contains("\"accept\":true");
                let reply = json!({"accepted": accept, "score": 1,
                                   "attempts": [{"score": 1, "digest": "d"}], "files": []})
                .to_string();
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        url
    }

    #[test]
    fn a_failed_boundary_keeps_the_contracts_agreed_and_does_not_unaccept_a_part() {
        let driver = accepting_driver();
        let parts = [
            Part { name: "a".into(), plan: json!({"accept": true}) },
            Part { name: "b".into(), plan: json!({"accept": false}) },
        ];
        let bounds = Bounds { branches: 1, max_rounds: 3, max_tokens: 0, patience: 0 };
        let c = compose_search(
            &driver,
            "h",
            &parts,
            "contract",
            1,
            bounds,
            0,
            Duration::from_secs(5),
            |_, _| default_strategies(1),
            // After round 0 the parts agree on v2 (so `a` runs again on it); after round 1 the
            // boundary fails and returns nothing.
            |r, _| match r {
                0 => vec![("a".into(), "contract".into(), 2), ("b".into(), "contract".into(), 2)],
                _ => vec![],
            },
        );
        let a = &c.parts[0];
        assert!(a.accepted);
        assert_eq!(a.rounds.len(), 2, "accepted on v2, it did not rerun after the failure");
        assert_eq!(c.contract_version, 2);
    }

    fn entry(name: &str, score: u64, files: Value) -> Entry {
        Entry {
            branch: name.into(),
            accepted: false,
            score,
            digest: format!("{name}-d"),
            spent_tokens: 10,
            attempts: 1,
            files,
            failures: json!([{ "id": "x", "detail": "not yet" }]),
            note: String::new(),
            elapsed_ms: 1,
            stopped: "exhausted".into(),
        }
    }

    #[test]
    fn the_first_branch_is_asked_exactly_what_the_goal_says() {
        assert_eq!(default_strategies(4)[0].lens, "");
    }

    #[test]
    fn branches_are_given_different_instructions() {
        let s = default_strategies(4);
        let mut lenses: Vec<&str> = s.iter().map(|x| x.lens.as_str()).collect();
        lenses.sort_unstable();
        lenses.dedup();
        assert_eq!(lenses.len(), 4, "two branches given the same lens differ only by seed");
    }

    #[test]
    fn exactly_one_branch_per_generation_reads_nothing() {
        for n in 2..=8u16 {
            let s = default_strategies(n);
            assert_eq!(
                s.iter().filter(|x| !x.reads_prior).count(),
                1,
                "with {n} branches, exactly one must start from the original tree"
            );
        }
        // With a single branch there is nothing to escape from.
        assert!(default_strategies(1)[0].reads_prior);
    }

    #[test]
    fn a_seeded_branch_is_shown_the_winner_and_what_was_wrong_with_it() {
        let base = json!({
            "text": "go", "context": [{ "path": "a.rs", "content": "old" }], "previous": [],
        });
        let prior = entry("w", 500, json!([{ "path": "a.rs", "content": "better" }]));
        let seeded = plan_for(&base, Some(&prior), &default_strategies(2)[0]);
        assert_eq!(seeded["context"][0]["content"], json!("better"));
        assert_eq!(
            seeded["previous"][0]["id"],
            json!("x"),
            "the code without the verdict is half of it"
        );
    }

    #[test]
    fn the_branch_that_reads_nothing_is_shown_the_original() {
        let base = json!({
            "text": "go", "context": [{ "path": "a.rs", "content": "old" }], "previous": [],
        });
        let prior = entry("w", 500, json!([{ "path": "a.rs", "content": "better" }]));
        let blind = default_strategies(2).pop().unwrap();
        assert!(!blind.reads_prior);
        let seeded = plan_for(&base, Some(&prior), &blind);
        assert_eq!(seeded["context"][0]["content"], json!("old"));
        assert_eq!(seeded["previous"], json!([]), "it is not told what it never saw");
    }

    #[test]
    fn a_file_the_winner_added_is_carried_forward() {
        let base = json!({ "text": "go", "context": [], "previous": [] });
        let prior = entry("w", 500, json!([{ "path": "new.rs", "content": "invented" }]));
        let seeded = plan_for(&base, Some(&prior), &default_strategies(1)[0]);
        assert_eq!(seeded["context"][0]["path"], json!("new.rs"));
    }

    fn part(name: &str, text: &str) -> Part {
        Part {
            name: name.into(),
            plan: json!({ "text": text, "context": [], "checks": [], "max_attempts": 1 }),
        }
    }

    #[test]
    fn the_contract_arrives_as_a_file_the_branch_may_not_edit() {
        let p = part("backend", "serve the search route");
        let out = with_contract(&p.plan, "{\"routes\":[\"/api/search\"]}", 3);
        let ctx = out["context"].as_array().unwrap();
        assert_eq!(ctx.len(), 1, "the contract is context, not a new plan field");
        assert_eq!(ctx[0]["path"], CONTRACT_PATH);
        assert!(ctx[0]["content"].as_str().unwrap().contains("/api/search"));
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("serve the search route"), "the goal survives: {text}");
        assert!(text.contains("contract v3"), "the version has to be auditable later: {text}");
        assert!(text.contains("Do NOT edit it"), "{text}");
    }

    #[test]
    fn a_moved_contract_replaces_the_old_one_rather_than_stacking() {
        let p = part("frontend", "render the results");
        let v3 = with_contract(&p.plan, "v3 body", 3);
        let v4 = with_contract(&v3, "v4 body", 4);
        let ctx = v4["context"].as_array().unwrap();
        assert_eq!(ctx.len(), 1, "two contracts in one tree is two answers to one question");
        assert_eq!(ctx[0]["content"], "v4 body");
    }

    #[test]
    fn parts_and_rounds_never_share_a_seed() {
        let seed_of = |part_index: u64, round: u64, branches: u64| {
            1000 + (part_index + 1) * PART_STRIDE + round * STRIDE * (branches + 1)
        };
        let mut seen = std::collections::HashSet::new();
        for p in 0..4 {
            for r in 0..20 {
                for b in 0..4 {
                    assert!(
                        seen.insert(seed_of(p, r, 4) + b * STRIDE),
                        "part {p} round {r} branch {b} reused a seed"
                    );
                }
            }
        }
    }

    fn outcome(part: &str, accepted: bool, score: u64, rounds: usize) -> PartOutcome {
        PartOutcome {
            part: part.into(),
            rounds: (0..rounds).map(|_| Round { entries: vec![], best: None }).collect(),
            best: (score > 0).then(|| entry("b0", score, json!([]))),
            accepted,
            spent_tokens: 0,
            built_against: 1,
        }
    }

    #[test]
    fn a_run_missing_one_half_says_which_half() {
        let c = Composition {
            parts: vec![outcome("backend", true, 1000, 2), outcome("frontend", false, 400, 2)],
            contract_version: 2,
            blocked: vec![],
            spent_tokens: 0,
            rounds_run: 2,
            stopped: SearchStop::Exhausted,
        };
        let blocked: Vec<String> = c
            .parts
            .iter()
            .filter(|o| !o.accepted)
            .map(|o| format!("{} never passed its gate", o.part))
            .collect();
        assert_eq!(blocked, ["frontend never passed its gate"]);
        let holed = Composition { blocked, ..c.clone() };
        assert!(!holed.accepted(), "a brilliant backend and no frontend is nothing");
        assert!(c.accepted(), "both halves green is the only acceptance");
        assert_eq!(c.winners().len(), 2, "the selector is handed one winner per part");
    }

    #[test]
    fn an_empty_composition_is_not_an_accepted_one() {
        let c = Composition {
            parts: vec![],
            contract_version: 1,
            blocked: vec![],
            spent_tokens: 0,
            rounds_run: 0,
            stopped: SearchStop::Exhausted,
        };
        assert!(!c.accepted(), "no parts is not the same as every part passing");
    }

    #[test]
    fn a_lesson_is_advice_and_the_goal_is_still_the_goal() {
        let s = Strategy {
            lens: "Prefer the smallest change.".into(),
            reads_prior: false,
            host: String::new(),
            knowledge: "- a bare unwrap fails the gate here".into(),
        };
        let text = plan_for(&json!({ "text": "make it work", "context": [] }), None, &s)["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.starts_with("make it work"), "the goal comes first: {text}");
        assert!(text.contains("Prefer the smallest change."), "the lens survives");
        assert!(text.contains("a bare unwrap fails the gate here"), "and so does the lesson");
        assert!(text.contains("Advice, not instructions"), "{text}");
        assert!(text.contains("judged by is the checks"), "{text}");
    }

    #[test]
    fn a_branch_that_reads_nothing_is_shown_nothing() {
        let cold = Strategy {
            lens: String::new(),
            reads_prior: false,
            host: String::new(),
            knowledge: String::new(),
        };
        let text = plan_for(&json!({ "text": "make it work", "context": [] }), None, &cold)["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(text, "make it work", "the control arm reads the goal and nothing else");
    }

    #[test]
    fn the_lens_is_appended_and_the_goal_survives_it() {
        let base = json!({ "text": "make it fast", "context": [], "previous": [] });
        let s = &default_strategies(2)[1];
        let p = plan_for(&base, None, s);
        let text = p["text"].as_str().unwrap();
        assert!(text.starts_with("make it fast"), "the goal must still be the goal: {text}");
        assert!(text.contains(&s.lens));
    }

    #[test]
    fn the_best_is_by_score_and_a_tie_goes_to_the_earlier_branch() {
        let e =
            [entry("a", 500, json!([])), entry("b", 900, json!([])), entry("c", 900, json!([]))];
        assert_eq!(best_of(&e), Some(1));
    }

    #[test]
    fn a_branch_that_never_ran_cannot_be_carried_forward() {
        let mut e = [entry("dead", 0, json!([])), entry("alive", 0, json!([]))];
        e[0].note = "unreachable".into();
        assert_eq!(best_of(&e), Some(1));
    }
}

// Decomposed goals (ADR-0086): competition inside a part, composition between parts.

/// Far enough that a part's rounds never walk into another part's seeds.
pub const PART_STRIDE: u64 = 1_000_000;

/// A file, not a plan field: the writer already renders `context`, so `graph:agent` is unchanged.
pub const CONTRACT_PATH: &str = "CONTRACT.md";

#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub plan: Value,
}

#[derive(Clone, Debug)]
pub struct PartOutcome {
    pub part: String,
    pub rounds: Vec<Round>,
    pub best: Option<Entry>,
    pub accepted: bool,
    pub spent_tokens: u64,
    /// What `composable` compares; parts that disagree must not be landed.
    pub built_against: u32,
}

#[derive(Clone, Debug)]
pub struct Composition {
    pub parts: Vec<PartOutcome>,
    pub contract_version: u32,
    /// One line per part that did not accept, so "no PR" says which half is missing.
    pub blocked: Vec<String>,
    pub spent_tokens: u64,
    pub rounds_run: u16,
    pub stopped: SearchStop,
}

impl Composition {
    pub fn accepted(&self) -> bool {
        self.blocked.is_empty() && !self.parts.is_empty()
    }

    pub fn winners(&self) -> Vec<(String, Entry)> {
        self.parts.iter().filter_map(|p| p.best.clone().map(|e| (p.part.clone(), e))).collect()
    }
}

/// Also said in the goal text: a model handed a file with no sentence about it will edit it.
pub fn with_contract(plan: &Value, body: &str, version: u32) -> Value {
    let mut plan = plan.clone();
    let text = plan["text"].as_str().unwrap_or_default().to_string();
    plan["text"] = json!(format!(
        "{text}\n\nThe interface for this work is in `{CONTRACT_PATH}` (contract v{version}). \
         Build against it exactly. Do NOT edit it — if it is wrong or missing something, \
         the other part has to agree to the change."
    ));
    let mut context = plan["context"].as_array().cloned().unwrap_or_default();
    let entry = json!({ "path": CONTRACT_PATH, "content": body });
    match context.iter_mut().find(|c| c["path"] == json!(CONTRACT_PATH)) {
        Some(existing) => *existing = entry,
        None => context.push(entry),
    }
    plan["context"] = Value::Array(context);
    plan
}

/// `boundary` runs between rounds and returns each part's contract. Nothing blocks inside a round,
/// so two parts waiting on each other is unreachable (ADR-0086).
// Each argument is a distinct axis of the search, not fields waiting to be a struct.
#[allow(clippy::too_many_arguments)]
pub fn compose_search<F, S>(
    driver_url: &str,
    host: &str,
    parts: &[Part],
    contract: &str,
    contract_version: u32,
    bounds: Bounds,
    base_seed: u64,
    timeout: Duration,
    mut strategies_for: S,
    mut boundary: F,
) -> Composition
where
    F: FnMut(u16, &[PartOutcome]) -> Vec<(String, String, u32)>,
    S: FnMut(&Part, u16) -> Vec<Strategy>,
{
    let mut outcomes: Vec<PartOutcome> = parts
        .iter()
        .map(|p| PartOutcome {
            part: p.name.clone(),
            rounds: Vec::new(),
            best: None,
            accepted: false,
            spent_tokens: 0,
            built_against: 0,
        })
        .collect();
    // They diverge only while one part demonstrates an amendment.
    let mut agreed: Vec<(String, String, u32)> =
        parts.iter().map(|p| (p.name.clone(), contract.to_string(), contract_version)).collect();
    let contract_of = |agreed: &[(String, String, u32)], part: &str| {
        agreed
            .iter()
            .find(|(n, _, _)| n == part)
            .map(|(_, b, v)| (b.clone(), *v))
            .unwrap_or_else(|| (contract.to_string(), contract_version))
    };
    let mut progress = Progress::new(bounds);
    let mut stopped = SearchStop::Exhausted;
    let mut rounds_run = 0u16;

    for r in 0..bounds.max_rounds {
        rounds_run = r + 1;
        // An accepted part does not rerun: its winner is what the others compose with.
        let running: Vec<usize> = (0..parts.len()).filter(|i| !outcomes[*i].accepted).collect();
        if running.is_empty() {
            stopped = SearchStop::Accepted;
            break;
        }

        // Concurrent like branches: parts are independent until the boundary.
        let handles: Vec<_> = running
            .iter()
            .map(|&i| {
                let url = driver_url.to_string();
                let host = host.to_string();
                let (body, version) = contract_of(&agreed, &parts[i].name);
                let strategies = strategies_for(&parts[i], r);
                let plan = with_contract(&parts[i].plan, &body, version);
                let carried = outcomes[i].best.clone();
                let seed = base_seed
                    + (i as u64 + 1) * PART_STRIDE
                    + (r as u64) * STRIDE * (bounds.branches as u64 + 1);
                std::thread::spawn(move || {
                    fan_out_from(&url, &host, &plan, &strategies, carried.as_ref(), seed, timeout)
                })
            })
            .collect();

        let mut improved_any = false;
        let mut round_spend = 0;
        for (&i, h) in running.iter().zip(handles) {
            let entries = h.join().unwrap_or_default();
            let part_spend: u64 = entries.iter().map(|e| e.spent_tokens).sum();
            round_spend += part_spend;
            outcomes[i].spent_tokens += part_spend;

            let winner = best_of(&entries);
            let improved = beats(outcomes[i].best.as_ref(), winner.map(|w| &entries[w]));
            let (_, version) = contract_of(&agreed, &parts[i].name);
            if improved {
                outcomes[i].best = winner.map(|w| entries[w].clone());
                outcomes[i].built_against = version;
                improved_any = true;
            }
            if entries.iter().any(|e| e.accepted) {
                outcomes[i].accepted = true;
                if let Some(a) = entries.iter().filter(|e| e.accepted).max_by_key(|e| e.score) {
                    outcomes[i].best = Some(a.clone());
                    outcomes[i].built_against = version;
                }
            }
            outcomes[i].rounds.push(Round { entries, best: winner });
        }

        let done = outcomes.iter().all(|o| o.accepted);
        if let Some(stop) = progress.after(round_spend, improved_any, done) {
            stopped = stop;
            break;
        }

        // Outstanding requests are resolved here and nowhere else. Nothing back is a boundary
        // that failed, and it keeps the contracts as agreed: it is not every part back on the
        // version the search began with.
        let next = boundary(r, &outcomes);
        if next.is_empty() {
            continue;
        }
        for (i, p) in parts.iter().enumerate() {
            let (_, was) = contract_of(&agreed, &p.name);
            let (_, now) = contract_of(&next, &p.name);
            // A candidate built against v3 is not a candidate for v4.
            if now != was {
                outcomes[i].accepted = false;
            }
        }
        agreed = next;
    }

    let blocked: Vec<String> = outcomes
        .iter()
        .filter(|o| !o.accepted)
        .map(|o| match &o.best {
            Some(b) => format!(
                "{} never passed its gate (best score {}, {} round(s))",
                o.part,
                b.score,
                o.rounds.len()
            ),
            None => format!("{} produced nothing in {} round(s)", o.part, o.rounds.len()),
        })
        .collect();

    // They agree unless a proposal is undemonstrated, which `composable` refuses at landing.
    let ended_on = agreed.first().map(|(_, _, v)| *v).unwrap_or(contract_version);
    Composition {
        parts: outcomes,
        contract_version: ended_on,
        blocked,
        spent_tokens: progress.spent,
        rounds_run,
        stopped,
    }
}
