//! `graph-selector` — which branch won, and the only path to a pull request.
//!
//! ## Rules, in order
//!
//! 1. **Accepted, or not eligible.** The gate is why a swarm can run unattended; a selection reaching past it would void that.
//! 2. **Higher score wins.** Branches passing every REQUIRED check differ on the optional ones, the only quality
//!    signal from real commands rather than a model's opinion.
//! 3. **Then the smaller change.** A judgement, so second: it must never override something a check verified.
//! 4. **Then the cheaper run**, then the earlier branch: tie-breaks whose job is to be DETERMINISTIC.
//!
//! It deliberately cannot see the attempt log: rewarding persistence would reward effort, not code.
//!
//! ## The Jev advisory
//!
//! `land` asks `jev:decision/decision::gate` one question about the winner AFTER `decide()` picked it: does
//! it look like a stub, a deletion of existing functionality, or a failure to implement the goal? NOT a
//! second selection signal (`decide()` is not passed the answer): a note on the PR, read by a person. A Jev
//! failure never blocks `land`; it becomes a dull line in the PR body.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::select::selector::{
    Chosen, Decision, Entry, File, Guest, LandError, Landing, Opened, Outcome, SelectError,
};
use bindings::git::forge::repo as forge;
use bindings::jev::decision::decision::{self as jev, DecisionError, GateRequest, GateResult};

struct Component;

/// The comparison, as a sort key. Lower is better everywhere, so `score` is negated by subtracting from
/// its maximum rather than reversing the sort, which would also reverse the tie-breaks.
fn rank(e: &Entry, index: usize) -> (u32, usize, u32, usize) {
    (1000u32.saturating_sub(e.score), e.files.len(), e.spent_tokens, index)
}

/// Why this one, said so a person can check it against the entries.
fn because(winner: &Entry, runner_up: Option<&Entry>) -> String {
    let Some(other) = runner_up else {
        return "the only branch that passed the gate".into();
    };
    if winner.score > other.score {
        format!("scored {} against {} for {}", winner.score, other.score, other.branch)
    } else if winner.files.len() < other.files.len() {
        format!(
            "a smaller change than {} at the same score: {} file(s) against {}",
            other.branch,
            winner.files.len(),
            other.files.len()
        )
    } else if winner.spent_tokens < other.spent_tokens {
        format!(
            "the same change as {} for less: {} tokens against {}",
            other.branch, winner.spent_tokens, other.spent_tokens
        )
    } else {
        // Nothing separated them: saying so is useful (two equally good branches) and invisible if only a winner is named.
        format!("indistinguishable from {}, and it came first", other.branch)
    }
}

/// Milli-probability, 0..=1000. At or above this, the advisory line becomes a
/// visible warning rather than a quiet confirmation.
const CONCERN_THRESHOLD: u32 = 500;

/// A cheap sanity check, not a code review: the winner's files are capped, since spotting a stub needs far less than every byte.
const STATE_BYTE_CAP: usize = 20_000;

/// The winning candidate's files, as the `state` a Noul question judges. Capped and truncated, and
/// truncation is said out loud so a truncated file is never mistaken for a short one.
fn advisory_state(files: &[File]) -> String {
    let mut out = String::new();
    for (i, f) in files.iter().enumerate() {
        let header = format!("=== {} ===\n", f.path);
        if out.len() + header.len() + f.content.len() > STATE_BYTE_CAP {
            out.push_str(&format!("=== truncated: {} more file(s) omitted ===\n", files.len() - i));
            break;
        }
        out.push_str(&header);
        out.push_str(&f.content);
        out.push('\n');
    }
    out
}

/// The Noul question `land` asks about the winner before it reaches the forge. `instructions` names the
/// goal from `p.title` (guaranteed short); `p.body` is the truncated fallback.
fn advisory_request(winner: &Entry, p: &Landing) -> GateRequest {
    let goal =
        if !p.title.is_empty() { p.title.clone() } else { p.body.chars().take(200).collect() };
    GateRequest {
        state: advisory_state(&winner.files),
        instructions: format!(
            "The goal was: {goal}. Does the code below look like a stub, a \
             deletion of existing functionality, or otherwise fail to \
             implement the goal — rather than a genuine, substantive attempt \
             at it?"
        ),
        true_hint: "The files are empty, trivially stubbed, mostly deletions, \
            or otherwise do not implement what the goal asked for."
            .to_string(),
        false_hint: "The files contain a genuine, substantive attempt to \
            implement the goal, even if imperfect."
            .to_string(),
    }
}

/// Exactly one line for the PR body. Never absent (a silent guard is not trusted) and never a reason to fail `land`.
fn advisory_line(result: Result<GateResult, DecisionError>) -> String {
    match result {
        Ok(r) if r.probability >= CONCERN_THRESHOLD => format!(
            "⚠ Jev advisory: this candidate may not fully implement the goal \
             (concern {:.0}%). Human review recommended before merging.",
            r.probability as f64 / 10.0
        ),
        Ok(r) => format!(
            "Jev advisory: looks like a substantive implementation (concern {:.0}%).",
            r.probability as f64 / 10.0
        ),
        Err(e) => {
            let detail = match e {
                DecisionError::InvalidRequest(m) => format!("invalid request: {m}"),
                DecisionError::ProviderDenied(m) => format!("provider denied: {m}"),
                DecisionError::ProviderUnavailable(m) => format!("provider unavailable: {m}"),
                DecisionError::BadResponse(m) => format!("bad response: {m}"),
            };
            format!("Jev advisory: unavailable ({detail}).")
        }
    }
}

/// Everything the caller gets, decided.
fn decide(entries: &[Entry]) -> Result<Outcome, SelectError> {
    if entries.is_empty() {
        return Err(SelectError::Invalid(
            "no branches — a generation of nothing has no winner".into(),
        ));
    }
    if let Some(bad) = entries.iter().find(|e| e.accepted && e.files.is_empty()) {
        // The forge refuses a diff-less proposal, but by then a branch is declared winner: refuse here so the generation fails, not the pull request.
        return Err(SelectError::Invalid(format!(
            "{} claims the gate accepted it and changed nothing, which is what \
             `reported success having done nothing` looks like",
            bad.branch
        )));
    }

    let mut distinct: Vec<&str> = entries.iter().map(|e| e.digest.as_str()).collect();
    distinct.sort_unstable();
    distinct.dedup();

    let common = Outcome {
        decision: Decision::NothingAcceptable(String::new()),
        distinct: distinct.len() as u32,
        accepted: entries.iter().filter(|e| e.accepted).count() as u32,
        spent_tokens: entries.iter().map(|e| e.spent_tokens).fold(0u32, |a, b| a.saturating_add(b)),
    };

    let mut eligible: Vec<(usize, &Entry)> =
        entries.iter().enumerate().filter(|(_, e)| e.accepted).collect();

    if eligible.is_empty() {
        let best = entries.iter().max_by_key(|e| e.score);
        let detail = match best {
            Some(b) => {
                format!("no branch passed the gate; the closest was {} at {}", b.branch, b.score)
            }
            None => "no branch passed the gate".into(),
        };
        return Ok(Outcome { decision: Decision::NothingAcceptable(detail), ..common });
    }

    eligible.sort_by_key(|(i, e)| rank(e, *i));
    let (index, winner) = eligible[0];
    let runner_up = eligible.get(1).map(|(_, e)| *e);

    Ok(Outcome {
        decision: Decision::Winner(Chosen {
            index: index as u32,
            branch: winner.branch.clone(),
            because: because(winner, runner_up),
        }),
        ..common
    })
}

impl Guest for Component {
    fn select(entries: Vec<Entry>) -> Result<Outcome, SelectError> {
        decide(&entries)
    }

    fn land(entries: Vec<Entry>, p: Landing) -> Result<Opened, LandError> {
        let outcome = decide(&entries).map_err(|SelectError::Invalid(m)| LandError::Invalid(m))?;

        let chosen = match outcome.decision {
            Decision::Winner(c) => c,
            // Not a forge failure: the caller's move is another generation, not a retry of this call.
            Decision::NothingAcceptable(why) => return Err(LandError::NothingAcceptable(why)),
        };
        let winner = &entries[chosen.index as usize];

        // Advisory-only, asked AFTER the winner is decided (see the module doc): it can add a PR line, not change `chosen`, `winner` or whether this happens.
        let advisory = advisory_line(jev::gate(&advisory_request(winner, &p)));

        // The changes come from the branch that won and nowhere else; a caller cannot supply them.
        let proposal = forge::Proposal {
            branch: p.branch,
            base: p.base,
            title: p.title,
            // What decided it travels with the proposal, so a reviewer can see that other branches tried and what they scored.
            body: format!(
                "{}\n\nSelected: {} — {}.\n{}",
                p.body, chosen.branch, chosen.because, advisory
            ),
            message: p.message,
            changes: winner
                .files
                .iter()
                .map(|f| forge::FileChange { path: f.path.clone(), content: f.content.clone() })
                .collect(),
        };

        forge::propose(&proposal).map_err(|e| {
            LandError::Forge(match e {
                forge::ForgeError::Rejected(m) => format!("rejected: {m}"),
                forge::ForgeError::Unavailable(m) => format!("unavailable: {m}"),
                forge::ForgeError::NotConfigured(m) => format!("not configured: {m}"),
                forge::ForgeError::Conflict(m) => format!("conflict: {m}"),
            })
        })
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(branch: &str, accepted: bool, score: u32, files: usize, tokens: u32) -> Entry {
        Entry {
            branch: branch.into(),
            accepted,
            score,
            digest: format!("{branch}-digest"),
            spent_tokens: tokens,
            attempts: 1,
            files: (0..files)
                .map(|n| File { path: format!("f{n}.rs"), content: format!("{branch}{n}") })
                .collect(),
        }
    }

    fn winner_of(entries: &[Entry]) -> Chosen {
        match decide(entries).unwrap().decision {
            Decision::Winner(c) => c,
            Decision::NothingAcceptable(w) => panic!("expected a winner, got: {w}"),
        }
    }

    /// The rule the whole design rests on: a branch that failed its checks is not a candidate however good it looks.
    #[test]
    fn a_branch_that_failed_the_gate_cannot_win_on_any_other_rule() {
        let w = winner_of(&[
            entry("reckless", false, 900, 1, 10),
            entry("careful", true, 600, 9, 9000),
        ]);
        assert_eq!(w.branch, "careful");
    }

    #[test]
    fn nothing_acceptable_says_how_close_it_got() {
        let out = decide(&[entry("a", false, 400, 1, 5), entry("b", false, 750, 1, 5)]).unwrap();
        match out.decision {
            Decision::NothingAcceptable(why) => {
                assert!(why.contains("750") && why.contains('b'), "{why}");
            }
            Decision::Winner(c) => panic!("nothing passed the gate, yet {} won", c.branch),
        }
        assert_eq!(out.accepted, 0);
    }

    /// Both passed every required check; the optional ones separate them.
    #[test]
    fn score_beats_a_smaller_change() {
        let w =
            winner_of(&[entry("small", true, 700, 1, 10), entry("thorough", true, 1000, 8, 10)]);
        assert_eq!(w.branch, "thorough");
        assert!(w.because.contains("1000") && w.because.contains("700"), "{}", w.because);
    }

    #[test]
    fn at_the_same_score_the_smaller_change_wins() {
        let w =
            winner_of(&[entry("sprawling", true, 1000, 6, 10), entry("tight", true, 1000, 2, 99)]);
        assert_eq!(w.branch, "tight", "same gate passed, less to review");
        assert!(w.because.contains("smaller"), "{}", w.because);
    }

    #[test]
    fn then_the_cheaper_run() {
        let w = winner_of(&[entry("dear", true, 1000, 2, 5000), entry("cheap", true, 1000, 2, 50)]);
        assert_eq!(w.branch, "cheap");
        assert!(w.because.contains("less"), "{}", w.because);
    }

    /// Determinism is the point of the last tie-break.
    #[test]
    fn a_total_tie_goes_to_the_earlier_branch_every_time() {
        let entries = [entry("first", true, 1000, 2, 10), entry("second", true, 1000, 2, 10)];
        for _ in 0..5 {
            assert_eq!(winner_of(&entries).branch, "first");
        }
        assert!(winner_of(&entries).because.contains("indistinguishable"));
    }

    /// Herding is invisible from anywhere else: eight branches that agreed look like eight that explored.
    #[test]
    fn a_generation_that_explored_one_idea_says_so() {
        let mut herd = [entry("a", true, 1000, 1, 10), entry("b", true, 1000, 1, 10)];
        herd[1].digest = herd[0].digest.clone();
        assert_eq!(decide(&herd).unwrap().distinct, 1, "the parallelism bought nothing");

        let spread = [entry("a", true, 1000, 1, 10), entry("b", true, 1000, 1, 10)];
        assert_eq!(decide(&spread).unwrap().distinct, 2);
    }

    #[test]
    fn the_generations_cost_is_the_sum_of_its_branches() {
        let out = decide(&[entry("a", true, 500, 1, 120), entry("b", false, 100, 1, 380)]).unwrap();
        assert_eq!(out.spent_tokens, 500, "including the branches that lost");
        assert_eq!(out.accepted, 1);
    }

    /// Caught here rather than at the forge, where a branch is already declared winner.
    #[test]
    fn an_accepted_branch_that_changed_nothing_is_refused() {
        let err = decide(&[entry("hollow", true, 1000, 0, 10)]).unwrap_err();
        let SelectError::Invalid(m) = err;
        assert!(m.contains("hollow"), "{m}");
    }

    #[test]
    fn a_generation_of_nothing_has_no_winner() {
        assert!(decide(&[]).is_err());
    }

    fn landing(title: &str, body: &str) -> Landing {
        Landing {
            branch: "b".into(),
            base: "main".into(),
            title: title.into(),
            body: body.into(),
            message: "m".into(),
        }
    }

    #[test]
    fn the_advisory_question_names_the_goal_from_the_title() {
        let winner = entry("tight", true, 1000, 1, 10);
        let req = advisory_request(&winner, &landing("add a cache", "long context nobody wants"));
        assert!(req.instructions.contains("add a cache"));
        assert!(!req.instructions.contains("long context"), "title wins over body");
    }

    #[test]
    fn the_advisory_question_falls_back_to_a_truncated_body_with_no_title() {
        let winner = entry("tight", true, 1000, 1, 10);
        let long_body = "x".repeat(500);
        let req = advisory_request(&winner, &landing("", &long_body));
        assert!(
            req.instructions.len() < long_body.len(),
            "the body must be truncated, not echoed whole"
        );
    }

    #[test]
    fn advisory_state_includes_every_file_under_the_cap() {
        let winner = entry("tight", true, 1000, 3, 10);
        let state = advisory_state(&winner.files);
        for f in &winner.files {
            assert!(state.contains(&f.path), "missing {}", f.path);
        }
    }

    #[test]
    fn advisory_state_truncates_past_the_byte_cap_and_says_so() {
        let files = vec![
            File { path: "a.rs".into(), content: "x".repeat(STATE_BYTE_CAP) },
            File { path: "b.rs".into(), content: "this must not appear".into() },
        ];
        let state = advisory_state(&files);
        assert!(!state.contains("this must not appear"));
        assert!(
            state.contains("truncated"),
            "a truncation must say so, not look like a short file"
        );
    }

    #[test]
    fn a_high_concern_probability_becomes_a_visible_warning() {
        let line = advisory_line(Ok(GateResult { probability: 900 }));
        assert!(line.starts_with('⚠'));
        assert!(line.contains("90%"));
    }

    #[test]
    fn a_low_concern_probability_is_a_quiet_confirmation() {
        let line = advisory_line(Ok(GateResult { probability: 50 }));
        assert!(!line.starts_with('⚠'));
        assert!(line.contains("substantive"));
    }

    #[test]
    fn exactly_at_the_threshold_counts_as_concerning() {
        let line = advisory_line(Ok(GateResult { probability: CONCERN_THRESHOLD }));
        assert!(line.starts_with('⚠'));
    }

    #[test]
    fn a_jev_failure_is_visible_but_never_an_error() {
        let line = advisory_line(Err(DecisionError::ProviderUnavailable("timed out".into())));
        assert!(line.contains("unavailable"));
        assert!(line.contains("timed out"));
    }
}
