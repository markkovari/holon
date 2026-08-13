//! `graph-selector` — which branch won, and the only path to a pull request.
//!
//! ## The order the rules are applied, and why each one
//!
//! 1. **Accepted, or not eligible.** The gate is the whole reason a swarm can be
//!    trusted to run unattended, and a selection that could reach past it would
//!    make that untrue whatever it reported afterwards.
//! 2. **Higher score wins.** Two branches can both pass every REQUIRED check and
//!    differ on the optional ones — that difference is the only quality signal
//!    that came from running real commands rather than from a model's opinion.
//! 3. **Then the smaller change.** Same gate passed, less to review and less to
//!    go wrong. This is a judgement rather than a measurement, and it is second
//!    because it should never override something a check actually verified.
//! 4. **Then the cheaper run**, then the earlier branch. Both are tie-breaks
//!    whose only real job is to be DETERMINISTIC: a selection that varies between
//!    runs of identical inputs cannot be argued with afterwards.
//!
//! ## What it deliberately cannot see
//!
//! The attempt log. A selector given "this branch tried nine times" would sooner
//! or later reward persistence, and how hard a branch worked is not a property of
//! the code it produced.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::select::selector::{
    Chosen, Decision, Entry, Guest, LandError, Landing, Opened, Outcome, SelectError,
};
use bindings::git::forge::repo as forge;

struct Component;

/// The comparison, as a sort key. Lower is better in every position, so `score`
/// is negated by subtracting from its maximum rather than by reversing the sort —
/// reversing would also reverse the tie-breaks, and the earlier branch would stop
/// winning ties.
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
        // Nothing separated them. Saying so is the useful answer — it means the
        // generation had two equally good branches, which is worth knowing and
        // is invisible if the report only names a winner.
        format!("indistinguishable from {}, and it came first", other.branch)
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
        // The forge refuses a diff-less proposal, but by then a branch has been
        // declared the winner. Refused here so the generation fails rather than
        // the pull request.
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
            Some(b) => format!(
                "no branch passed the gate; the closest was {} at {}",
                b.branch, b.score
            ),
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
            // Not a forge failure, and kept apart from one: the caller's move is
            // to run another generation, not to retry this call.
            Decision::NothingAcceptable(why) => return Err(LandError::NothingAcceptable(why)),
        };
        let winner = &entries[chosen.index as usize];

        // The changes come from the branch that won, and from nowhere else.
        // There is no argument by which a caller could supply them.
        let proposal = forge::Proposal {
            branch: p.branch,
            base: p.base,
            title: p.title,
            // What decided it travels with the proposal. A reviewer looking at
            // one pull request cannot otherwise see that seven other branches
            // tried and what they scored.
            body: format!("{}\n\nSelected: {} — {}.", p.body, chosen.branch, chosen.because),
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
    use bindings::exports::graph::select::selector::File;

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

    /// The rule the whole design rests on. A branch that failed its checks is not
    /// a candidate however good it looks on every other axis.
    #[test]
    fn a_branch_that_failed_the_gate_cannot_win_on_any_other_rule() {
        let w = winner_of(&[
            // Higher score, smaller change, cheaper — and rejected.
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

    /// Both passed every required check; the optional ones separate them, and
    /// that difference came from running real commands.
    #[test]
    fn score_beats_a_smaller_change() {
        let w = winner_of(&[entry("small", true, 700, 1, 10), entry("thorough", true, 1000, 8, 10)]);
        assert_eq!(w.branch, "thorough");
        assert!(w.because.contains("1000") && w.because.contains("700"), "{}", w.because);
    }

    #[test]
    fn at_the_same_score_the_smaller_change_wins() {
        let w = winner_of(&[entry("sprawling", true, 1000, 6, 10), entry("tight", true, 1000, 2, 99)]);
        assert_eq!(w.branch, "tight", "same gate passed, less to review");
        assert!(w.because.contains("smaller"), "{}", w.because);
    }

    #[test]
    fn then_the_cheaper_run() {
        let w = winner_of(&[entry("dear", true, 1000, 2, 5000), entry("cheap", true, 1000, 2, 50)]);
        assert_eq!(w.branch, "cheap");
        assert!(w.because.contains("less"), "{}", w.because);
    }

    /// Determinism is the point of the last tie-break. A selection that varied
    /// between runs of identical input could not be argued with afterwards.
    #[test]
    fn a_total_tie_goes_to_the_earlier_branch_every_time() {
        let entries = [entry("first", true, 1000, 2, 10), entry("second", true, 1000, 2, 10)];
        for _ in 0..5 {
            assert_eq!(winner_of(&entries).branch, "first");
        }
        assert!(winner_of(&entries).because.contains("indistinguishable"));
    }

    /// Herding is invisible from anywhere else: eight branches that agreed look
    /// exactly like eight branches that explored.
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

    /// Caught here rather than at the forge, where a branch has already been
    /// declared the winner.
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
}
