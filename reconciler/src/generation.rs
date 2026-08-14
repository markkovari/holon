//! A generation: one goal, N branches at once, one winner.
//!
//! ## Why this is native and not a component
//!
//! Because it is the part that has to happen CONCURRENTLY. A component runs one
//! call at a time; a generation whose branches ran in sequence would take N times
//! as long as its slowest branch and would not be a generation at all — it would
//! be a loop with extra vocabulary. The fan-out is threads and sockets, so it
//! lives where threads and sockets live, and every decision it makes is delegated
//! to a component that can be reasoned about: the driver decides when a branch
//! stops, the selector decides which branch won.
//!
//! ## Seeds are spaced, not consecutive
//!
//! Attempt `n` of a branch uses `seed + n`, so branches seeded one apart would
//! share prompts: branch 1's second attempt and branch 2's first would ask the
//! same question with the same seed. `STRIDE` keeps them apart, and it is far
//! larger than any sane `max-attempts` so the overlap cannot creep back.
//!
//! ## Diversity is authored, not hoped for
//!
//! Branches that differ only by SEED share a prompt, a context and a model, and a
//! model asked the same question four times answers it four similar ways. That is
//! herding, and it produces a healthy-looking generation whose parallelism bought
//! nothing. Each branch therefore gets a LENS — a different instruction appended
//! to the goal — and one branch in every generation is shown nothing from the
//! previous one (ADR-0081's "asymmetric visibility" and "one branch that reads
//! nothing"). The second matters most in a search: once a generation is seeded
//! from the last winner, every branch that reads it inherits its mistakes, and the
//! branch that reads nothing is the only one that can leave a local optimum.
//!
//! ## What a failed branch is
//!
//! An ordinary result. A branch that could not reach its provider still returns
//! an entry — unaccepted, with the error in `note` — because a generation of four
//! in which one died is a generation of three, not a failure. Dropping it
//! silently would make `distinct` and the total cost quietly wrong.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The gap between one branch's seed and the next.
///
/// Attempt `n` uses `seed + n`. Anything smaller than `max-attempts` makes two
/// branches ask an identical question, which is the one thing a generation is
/// for avoiding.
pub const STRIDE: u64 = 100;

/// What one branch came back with, in the shape `graph:select` wants.
#[derive(Clone, Debug)]
pub struct Entry {
    pub branch: String,
    pub accepted: bool,
    pub score: u64,
    pub digest: String,
    pub spent_tokens: u64,
    pub attempts: u64,
    pub files: Value,
    /// What was still failing. Carried into the NEXT generation, so branches
    /// seeded with this candidate are told what is wrong with it rather than
    /// being handed broken code and left to work it out.
    pub failures: Value,
    /// Why this branch produced nothing, when it produced nothing.
    pub note: String,
    /// How long this branch took on its own.
    ///
    /// The only way to tell a fan-out from a for-loop after the fact: run in
    /// parallel the wall clock is about the slowest branch, run in sequence it is
    /// about the SUM. Counting attempts cannot distinguish them, which a
    /// deliberately sequential version of `fan_out` proved by passing.
    pub elapsed_ms: u64,
    /// What the driver reported as its reason for stopping. Not sent to the
    /// selector — how a branch ended is not a property of the code it wrote —
    /// but the most useful thing in the log when a generation finds nothing.
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

/// Run one branch and turn whatever came back into an entry.
fn one_branch(url: &str, host: &str, plan: &Value, name: &str, seed: u64, timeout: Duration) -> Entry {
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
        // A branch that could not be reached is a branch that found nothing. The
        // generation carries on with the rest — which is the entire argument for
        // running more than one.
        Err(e) => return blank(e, "unreachable"),
    };
    if let Some(err) = answer["error"].as_str() {
        return blank(
            format!("{err}: {}", answer["detail"].as_str().unwrap_or_default()),
            err,
        );
    }

    // The digest of the run's BEST candidate, which is what the selector compares
    // across branches. The driver reports one per attempt; the one that matters
    // is the attempt whose score the run kept.
    let attempts = answer["attempts"].as_array().cloned().unwrap_or_default();
    let score = answer["score"].as_u64().unwrap_or(0);
    let digest = attempts
        .iter()
        .find(|a| a["score"].as_u64() == Some(score) && !a["digest"].as_str().unwrap_or("").is_empty())
        .and_then(|a| a["digest"].as_str())
        .unwrap_or_default()
        .to_string();

    // A branch that accepted nothing and produced no candidate has its reason in
    // the per-attempt `error` (the agent said something that was not a candidate).
    // Surface the last one as the note, so a run where every branch was rejected
    // by the agent is diagnosable instead of a wall of silent zeros.
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

/// How one branch is asked to differ from its siblings.
#[derive(Clone, Debug)]
pub struct Strategy {
    /// Appended to the goal. A different instruction is the only thing that makes
    /// two branches genuinely explore rather than sample the same answer twice.
    pub lens: String,
    /// Whether this branch is shown the previous generation's best candidate.
    ///
    /// Which door this branch answers on. Empty means the caller's default.
    ///
    /// Per branch because a branch may be its own ENVIRONMENT — a derived app
    /// with its own store (ADR-0078) and its own derived hostname (ADR-0083) —
    /// and then "which branch" and "which address" are the same question.
    pub host: String,
    /// `false` for exactly one branch per generation. Every branch that reads the
    /// last winner inherits its mistakes along with its progress, so a search in
    /// which they all do cannot leave a local optimum — the one branch that starts
    /// from the original tree is the only escape, and it costs one branch of the
    /// generation to have.
    pub reads_prior: bool,
}

/// The lenses, in the order branches get them.
///
/// The first is EMPTY on purpose: a control branch, asked exactly what the goal
/// says. If every branch is steered, none of them is answering the question as
/// written, and a lens that turns out to hurt would be invisible.
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

/// `n` strategies, distinct as far as the lens list goes, with exactly one branch
/// that reads nothing from the previous generation.
///
/// The reader-of-nothing is the LAST branch rather than the first, so the control
/// branch (lens 0, empty) is the one that carries the search forward.
pub fn default_strategies(n: u16) -> Vec<Strategy> {
    (0..n as usize)
        .map(|i| Strategy {
            // Wraps if somebody asks for more branches than there are lenses.
            // Duplicated lenses still differ by seed, which is weaker than a
            // distinct lens and better than nothing.
            lens: LENSES[i % LENSES.len()].to_string(),
            reads_prior: !(n > 1 && i == n as usize - 1),
            host: String::new(),
        })
        .collect()
}

/// Point each branch at its own environment.
///
/// `hosts[i]` is where branch `i` runs. Shorter than the strategy list is an
/// error rather than a fallback to the shared host: a branch that silently ran in
/// the wrong environment would write another branch's store, and the whole reason
/// for environments is that it cannot.
pub fn on_hosts(strategies: &[Strategy], hosts: &[String]) -> Vec<Strategy> {
    assert_eq!(
        strategies.len(),
        hosts.len(),
        "every branch needs its own environment, or one of them writes another's store"
    );
    strategies
        .iter()
        .zip(hosts)
        .map(|(s, h)| Strategy { host: h.clone(), ..s.clone() })
        .collect()
}

/// Apply a strategy to a plan: the lens onto the goal, and the previous
/// generation's work onto the context — or not, for the branch that reads
/// nothing.
fn plan_for(base: &Value, prior: Option<&Entry>, s: &Strategy) -> Value {
    let mut plan = base.clone();
    if !s.lens.is_empty() {
        let text = plan["text"].as_str().unwrap_or_default().to_string();
        plan["text"] = json!(format!("{text}\n\n{}", s.lens));
    }
    let Some(prior) = prior.filter(|_| s.reads_prior) else { return plan };

    // The winner's files laid over the goal's, exactly as the driver lays a
    // repair over an attempt: same rule, one level up.
    let mut context = plan["context"].as_array().cloned().unwrap_or_default();
    for f in prior.files.as_array().cloned().unwrap_or_default() {
        match context.iter_mut().find(|c| c["path"] == f["path"]) {
            Some(existing) => existing["content"] = f["content"].clone(),
            None => context.push(f),
        }
    }
    plan["context"] = Value::Array(context);
    // And what is still wrong with it. Seeding the code without the verdict hands
    // four branches broken work and does not say why.
    plan["previous"] = prior.failures.clone();
    plan
}

/// Fan one plan out to `branches` branches and wait for all of them.
///
/// Every branch is waited for, including the slow ones. Taking the first N to
/// finish would systematically prefer the branches that gave up early, which is
/// the opposite of what a search wants.
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

/// The same, with the strategies named and an optional candidate to build on.
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
            // The branch's own environment when it has one, the shared door
            // otherwise.
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

/// Hand a generation's entries to the selector, which decides and proposes.
///
/// The entries go across whole. Filtering the unaccepted ones out here would put
/// the gate in two places, and the one that could be forgotten is this one.
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

// ---- the search ------------------------------------------------------------

/// Why a search ended.
///
/// The same distinction the driver makes one level down, for the same reason: a
/// search that ran out of money and a search that ran out of ideas are different
/// facts, and only one of them is worth more money.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStop {
    /// A branch passed the gate. The only successful ending.
    Accepted,
    /// `max_rounds` generations produced nothing acceptable.
    Exhausted,
    /// `patience` generations in a row failed to beat the best score. The
    /// search is circling, and another generation costs a generation.
    NoProgress,
    /// The token budget is spent.
    OverBudget,
}

/// One generation's worth of the search.
#[derive(Clone, Debug)]
pub struct Round {
    pub entries: Vec<Entry>,
    /// Index into `entries` of the candidate carried into the next generation.
    pub best: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Search {
    pub rounds: Vec<Round>,
    pub accepted: bool,
    pub best: Option<Entry>,
    pub spent_tokens: u64,
    pub stopped: SearchStop,
}

/// How a search is bounded.
///
/// All three are needed and none replaces another: rounds bound the search when
/// every generation is cheap, the budget bounds it when they are not, and
/// patience stops one that is neither expensive nor going anywhere.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub branches: u16,
    pub max_rounds: u16,
    /// Across the whole search, every branch of every generation. 0 is unbounded.
    ///
    /// A budget PER BRANCH is what the driver already enforces, and four branches
    /// each inside their budget can put a project far outside its own.
    pub max_tokens: u64,
    /// Generations in a row that may fail to beat the best score. 0 disables it.
    pub patience: u16,
}

/// Pick the candidate a generation carries forward.
///
/// By score, and the earlier branch on a tie — the same rule the selector uses
/// between branches, because a search that carried one candidate forward and
/// proposed a different one would be optimising for something it does not land.
pub fn best_of(entries: &[Entry]) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.note.is_empty())
        .max_by_key(|(i, e)| (e.score, std::cmp::Reverse(*i)))
        .map(|(i, _)| i)
}

/// Run generations until one is accepted, or until a bound says stop.
///
/// Every generation after the first is SEEDED with the last one's best candidate
/// and the failures it still had — which is the only reason a search finds things
/// a single generation cannot. Except for the one branch per generation that
/// reads nothing, which is the only reason it can leave a local optimum.
pub fn search(
    driver_url: &str,
    host: &str,
    plan: &Value,
    bounds: Bounds,
    base_seed: u64,
    timeout: Duration,
) -> Search {
    let strategies = default_strategies(bounds.branches);
    let mut rounds: Vec<Round> = Vec::new();
    let mut carried: Option<Entry> = None;
    let mut best: Option<Entry> = None;
    let mut spent: u64 = 0;
    let mut stale: u16 = 0;

    let mut stopped = SearchStop::Exhausted;
    for r in 0..bounds.max_rounds {
        // Rounds are spaced far enough apart that generation two's first branch
        // cannot repeat a seed generation one already used.
        let seed = base_seed + (r as u64) * STRIDE * (bounds.branches as u64 + 1);
        let entries =
            fan_out_from(driver_url, host, plan, &strategies, carried.as_ref(), seed, timeout);

        spent += entries.iter().map(|e| e.spent_tokens).sum::<u64>();
        let winner = best_of(&entries);
        let improved = match (&best, winner.map(|i| &entries[i])) {
            (None, Some(_)) => true,
            (Some(b), Some(w)) => w.score > b.score,
            _ => false,
        };
        if improved {
            best = winner.map(|i| entries[i].clone());
            stale = 0;
        } else {
            stale += 1;
        }
        // Carried forward is the best SO FAR, not this round's best: a generation
        // that went backwards must not drag the next one down with it.
        carried = best.clone();

        let any_accepted = entries.iter().any(|e| e.accepted);
        rounds.push(Round { entries, best: winner });

        if any_accepted {
            stopped = SearchStop::Accepted;
            break;
        }
        // Both checked after the generation, because what one costs is not known
        // until it is run — the same overshoot the driver documents, one level up.
        if bounds.max_tokens > 0 && spent >= bounds.max_tokens {
            stopped = SearchStop::OverBudget;
            break;
        }
        if bounds.patience > 0 && stale >= bounds.patience {
            stopped = SearchStop::NoProgress;
            break;
        }
    }

    // The accepted candidate, not merely the highest-scoring one. They are
    // usually the same and the exception is the one that matters: a branch can
    // score well on optional checks while failing a required one.
    let accepted = rounds
        .iter()
        .flat_map(|r| r.entries.iter())
        .filter(|e| e.accepted)
        .max_by_key(|e| e.score)
        .cloned();
    Search {
        accepted: accepted.is_some(),
        best: accepted.or(best),
        rounds,
        spent_tokens: spent,
        stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Every branch steered is no control at all: a lens that turns out to hurt
    /// would be invisible if nothing was asked the goal as written.
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

    /// The escape hatch. Once a generation is seeded from the last winner, every
    /// branch that reads it inherits its mistakes.
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
        // With a single branch there is nothing to escape from, and a lone branch
        // that read nothing would make a search of one impossible.
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
        assert_eq!(seeded["previous"][0]["id"], json!("x"), "the code without the verdict is half of it");
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

    /// A file the winner invented has to reach the next generation, or every
    /// branch creates it again.
    #[test]
    fn a_file_the_winner_added_is_carried_forward() {
        let base = json!({ "text": "go", "context": [], "previous": [] });
        let prior = entry("w", 500, json!([{ "path": "new.rs", "content": "invented" }]));
        let seeded = plan_for(&base, Some(&prior), &default_strategies(1)[0]);
        assert_eq!(seeded["context"][0]["path"], json!("new.rs"));
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
        let e = [entry("a", 500, json!([])), entry("b", 900, json!([])), entry("c", 900, json!([]))];
        assert_eq!(best_of(&e), Some(1));
    }

    /// A branch that never ran has no score to compare, and treating its 0 as a
    /// score would let a dead branch win a generation in which everything failed.
    #[test]
    fn a_branch_that_never_ran_cannot_be_carried_forward() {
        let mut e = [entry("dead", 0, json!([])), entry("alive", 0, json!([]))];
        e[0].note = "unreachable".into();
        assert_eq!(best_of(&e), Some(1));
    }
}
