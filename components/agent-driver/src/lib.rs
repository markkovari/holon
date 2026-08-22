//! `agent-driver` — the loop that joins the writer to the gate.
//!
//! ## Every decision here is about when to stop
//!
//! The writer produces candidates and the evaluator scores them; neither has any
//! opinion about how many to make. That opinion is small enough to read in one
//! sitting and is the whole of this component:
//!
//!   * a candidate the gate accepts ends the run — nothing scores higher than
//!     acceptable, and continuing would be paying to look for a better `true`
//!   * a candidate identical to one already tried ends the run as a `plateau`,
//!     because a model that has repeated itself will repeat itself again and the
//!     next attempt costs the same as the last for an answer already on record
//!   * `patience` attempts in a row fail to beat the best score, and the run
//!     stops as `no-progress` — the common case, since a model rarely repeats
//!     itself exactly but routinely produces a different candidate that is no
//!     better
//!   * the token budget is spent, and the run stops as `over-budget`
//!   * otherwise the bound in `max-attempts` runs out, and `exhausted` says so
//!
//! ## A repair repairs the candidate, not the base
//!
//! Each attempt after the first is shown the BEST candidate so far in place of
//! the original files. Without that, "do not start over" is a request the writer
//! cannot honour: the model would be looking at the untouched tree every time and
//! could only rewrite from scratch, which makes a repair loop a re-roll with
//! hints attached.
//!
//! The BEST rather than the LAST, for the same reason the result keeps the best:
//! a repair can be worse than what it repaired, and building the next attempt on
//! it walks the search backwards.
//!
//! ## The two failures that are not the same failure
//!
//! An unusable ANSWER is retried with the next seed: the model was reachable and
//! said something that was not a candidate, which another sample may well fix.
//! A provider that is DOWN ends the run immediately — spending the remaining
//! attempts against it converts a budget into nothing, and the caller's move is
//! to wait rather than to try harder.
//!
//! ## Failures replace, they do not accumulate
//!
//! Each repair is told what failed for the candidate it is being shown, not
//! everything that has ever failed. An accumulating list grows the prompt without
//! bound and re-reports things already fixed, which reads to a model as a fix
//! that did not work.
//!
//! ## The budget has a hole, and it is bounded
//!
//! Cost is reported WITH a candidate, so an answer that was unusable — prose, or
//! a path outside the allow-list — cost real tokens that `spent-tokens` never
//! sees. `max-attempts` is what bounds that, which is why a token budget does not
//! replace it. Said out loud in the contract rather than papered over, because a
//! budget that silently under-reports is worse than no budget.
//!
//! ## What is deliberately NOT here
//!
//! Choosing between branches, and opening a pull request. A run is one line of
//! attempts; the winner of a generation is chosen by comparing runs, and a
//! driver that proposed its own result would open N pull requests for N branches
//! and hand the choosing to a human who now has N of them to read.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::run::driver::{
    Attempt, Failure, File, Goal, Guest, Plan, RunError, RunResult, StopReason,
};
use bindings::graph::agent::writer as agent;
use bindings::graph::fitness::evaluator as gate;

use sha2::{Digest, Sha256};

struct Component;

/// Name a candidate by its content.
///
/// Length-prefixed per field, for the reason every content address in this repo
/// is: concatenating `("ab", "c")` and `("a", "bc")` produces the same bytes, and
/// two different candidates that hash alike would read as a plateau that is not
/// one.
fn digest_of(files: &[File]) -> String {
    let mut h = Sha256::new();
    for f in files {
        h.update((f.path.len() as u64).to_le_bytes());
        h.update(f.path.as_bytes());
        h.update((f.content.len() as u64).to_le_bytes());
        h.update(f.content.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// What the repair is told: the checks that failed, and what they said.
///
/// The checks that PASSED are dropped. A model handed the whole report spends
/// its attention on the parts that are already fine, and the passing checks are
/// exactly the ones a repair must not disturb — which is what "do not start
/// over" in the writer's prompt is for.
fn failures_of(outcomes: &[gate::Outcome]) -> Vec<Failure> {
    outcomes
        .iter()
        .filter(|o| !o.passed)
        .map(|o| Failure { id: o.id.clone(), detail: o.detail.clone() })
        .collect()
}

/// Convert between two identical records that belong to different contracts.
///
/// Kept explicit rather than papered over with a shared type: the agent's file
/// and the gate's file mean different things — one is what a model proposes, the
/// other is what will be written to a tree — and the day one grows a field the
/// other should not, this is where it is noticed.
fn as_gate_files(files: &[File]) -> Vec<gate::File> {
    files.iter().map(|f| gate::File { path: f.path.clone(), content: f.content.clone() }).collect()
}

/// What the model is shown: the goal's files, with the best candidate's version
/// of each laid over the top.
///
/// This is what makes the loop a repair loop. The first attempt sees the base
/// tree; every one after it sees the best thing the run has produced, so
/// "fix these specifically, do not start over" refers to something that exists.
///
/// A file the candidate created and the goal never mentioned is APPENDED rather
/// than dropped — the next attempt has to see it or it will create it again,
/// which is how a loop produces the same new file three times.
fn goal_for(g: &Goal, best: Option<&Vec<File>>) -> agent::Goal {
    let mut context: Vec<agent::File> = g
        .context
        .iter()
        .map(|f| agent::File { path: f.path.clone(), content: f.content.clone() })
        .collect();

    for f in best.map(|b| b.as_slice()).unwrap_or(&[]) {
        match context.iter_mut().find(|c| c.path == f.path) {
            Some(existing) => existing.content = f.content.clone(),
            None => context.push(agent::File { path: f.path.clone(), content: f.content.clone() }),
        }
    }

    agent::Goal { text: g.text.clone(), context, writable: g.writable.clone() }
}

impl Guest for Component {
    fn run(p: Plan) -> Result<RunResult, RunError> {
        if p.checks.is_empty() {
            // The evaluator refuses this too. Refused here as well so a caller
            // learns before paying for inference, rather than after.
            return Err(RunError::Invalid(
                "no checks — an empty gate accepts everything, so a run against one would \
                 succeed on its first attempt and mean nothing"
                    .into(),
            ));
        }
        if p.max_attempts == 0 {
            return Err(RunError::Invalid("max-attempts is zero, so this run does nothing".into()));
        }

        let checks: Vec<gate::Check> = p
            .checks
            .iter()
            .map(|c| gate::Check {
                id: c.id.clone(),
                required: c.required,
                weight: c.weight,
                command: c.command.clone(),
            })
            .collect();

        let mut attempts: Vec<Attempt> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        // Seeded from the plan, so a generation can inherit what the last one
        // could not fix. Empty on a first generation.
        let mut previous: Vec<agent::Failure> = p
            .previous
            .iter()
            .map(|f| agent::Failure { id: f.id.clone(), detail: f.detail.clone() })
            .collect();
        let mut best: Option<(u32, Vec<File>, Vec<Failure>)> = None;
        let mut spent: u32 = 0;
        // Attempts in a row that failed to beat the best score.
        let mut stale: u32 = 0;
        // The runner caches base trees by commit. It is sent once and only sent
        // again if the runner says it has forgotten — a generation of branches
        // sharing one base should not each ship a repository.
        let mut base_known = false;

        let finish = |stopped: StopReason,
                      best: Option<(u32, Vec<File>, Vec<Failure>)>,
                      attempts: Vec<Attempt>,
                      spent: u32| {
            let (score, files, failures) = best.unwrap_or((0, Vec::new(), Vec::new()));
            RunResult {
                accepted: matches!(stopped, StopReason::Accepted),
                score,
                files,
                failures,
                attempts,
                spent_tokens: spent,
                stopped,
            }
        };

        for i in 0..p.max_attempts {
            let seed = p.seed.wrapping_add(i as u64);

            // Built per attempt, so a repair is shown the best candidate rather
            // than the untouched tree.
            let goal = goal_for(&p.goal, best.as_ref().map(|(_, f, _)| f));

            let produced = match agent::attempt(&goal, &previous, seed) {
                Ok(c) => c,
                // Reachable, and said something that was not a candidate. Another
                // sample may well be one, so this costs an attempt and not the run.
                Err(agent::AgentError::UnusableAnswer(m)) => {
                    // Cost travels with a candidate, and there is none. What this
                    // burned is invisible to `spent`; `max-attempts` is what
                    // bounds it.
                    attempts.push(Attempt {
                        seed,
                        digest: String::new(),
                        score: 0,
                        accepted: false,
                        error: m,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        model: String::new(),
                    });
                    continue;
                }
                // Not reachable. Every remaining attempt would fail the same way.
                Err(agent::AgentError::InferenceFailed(m)) => return Err(RunError::ProviderDown(m)),
                // The goal itself is wrong, and no seed fixes a goal.
                Err(agent::AgentError::UnderSpecified(m)) => return Err(RunError::Invalid(m)),
            };

            let cost = produced.prompt_tokens.saturating_add(produced.completion_tokens);
            spent = spent.saturating_add(cost);
            let files: Vec<File> = produced
                .files
                .into_iter()
                .map(|f| File { path: f.path, content: f.content })
                .collect();

            let digest = digest_of(&files);
            if seen.contains(&digest) {
                attempts.push(Attempt {
                    seed,
                    digest,
                    score: 0,
                    accepted: false,
                    error: "the same candidate an earlier attempt already produced".into(),
                    prompt_tokens: produced.prompt_tokens,
                    completion_tokens: produced.completion_tokens,
                    model: produced.model,
                });
                return Ok(finish(StopReason::Plateau, best, attempts, spent));
            }
            seen.push(digest.clone());

            // Judge it, answering `need-base` by sending the tree. Bounded at two
            // tries: a runner that asks for a base it was just handed is broken,
            // and looping on it would spend the run on one candidate.
            let mut verdict = None;
            let mut asked = String::new();
            for _ in 0..2 {
                let candidate = gate::Candidate {
                    name: format!("attempt-{i}"),
                    base_commit: p.base_commit.clone(),
                    base_tree: if base_known { Vec::new() } else { as_gate_files(&p.base_tree) },
                    changes: as_gate_files(&files),
                };
                match gate::evaluate(&candidate, &checks) {
                    Ok(v) => {
                        base_known = true;
                        verdict = Some(v);
                        break;
                    }
                    Err(gate::EvalError::NeedBase(c)) => {
                        if p.base_tree.is_empty() {
                            return Err(RunError::GateUnusable(format!(
                                "the runner has not seen base {c} and this plan carries no tree \
                                 to send it"
                            )));
                        }
                        base_known = false;
                        asked = c;
                    }
                    Err(gate::EvalError::Unavailable(m)) => return Err(RunError::GateUnusable(m)),
                    Err(gate::EvalError::Invalid(m)) => return Err(RunError::GateUnusable(m)),
                }
            }
            let Some(v) = verdict else {
                return Err(RunError::GateUnusable(format!(
                    "the runner kept asking for base {asked} after it was sent"
                )));
            };

            let failures = failures_of(&v.outcomes);
            attempts.push(Attempt {
                seed,
                digest,
                score: v.score,
                accepted: v.accepted,
                error: String::new(),
                prompt_tokens: produced.prompt_tokens,
                completion_tokens: produced.completion_tokens,
                model: produced.model,
            });

            // Strictly better, so the FIRST attempt to reach a score keeps the
            // slot. A later attempt that merely ties has not improved on it, and
            // preferring the newer one would make a run's answer depend on how
            // many times it happened to tie.
            if best.as_ref().is_none_or(|(s, _, _)| v.score > *s) {
                best = Some((v.score, files, failures.clone()));
                stale = 0;
            } else {
                stale += 1;
            }

            if v.accepted {
                return Ok(finish(StopReason::Accepted, best, attempts, spent));
            }

            // Replaced, not appended — see the module comment. Taken from THIS
            // attempt's verdict even when the attempt was not an improvement,
            // because the next one is shown the best candidate and must be told
            // what is wrong with THAT.
            previous = best
                .as_ref()
                .map(|(_, _, f)| f.clone())
                .unwrap_or(failures)
                .iter()
                .map(|f| agent::Failure { id: f.id.clone(), detail: f.detail.clone() })
                .collect();

            // Both checked AFTER the attempt: what one costs is not knowable
            // until it is made, so a run overshoots by at most one rather than
            // guessing a price in advance and calling the guess a limit.
            if p.max_tokens > 0 && spent >= p.max_tokens {
                return Ok(finish(StopReason::OverBudget, best, attempts, spent));
            }
            if p.patience > 0 && stale >= p.patience {
                return Ok(finish(StopReason::NoProgress, best, attempts, spent));
            }
        }

        Ok(finish(StopReason::Exhausted, best, attempts, spent))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, content: &str) -> File {
        File { path: path.into(), content: content.into() }
    }

    fn outcome(id: &str, passed: bool, detail: &str) -> gate::Outcome {
        gate::Outcome {
            id: id.into(),
            required: true,
            weight: 1,
            passed,
            took_ms: 0,
            detail: detail.into(),
        }
    }

    #[test]
    fn the_same_candidate_digests_the_same_and_a_different_one_does_not() {
        let a = vec![f("src/lib.rs", "42")];
        assert_eq!(digest_of(&a), digest_of(&vec![f("src/lib.rs", "42")]));
        assert_ne!(digest_of(&a), digest_of(&vec![f("src/lib.rs", "41")]));
        assert_ne!(digest_of(&a), digest_of(&vec![f("src/other.rs", "42")]));
    }

    /// The reason every field is length-prefixed. Without it these two candidates
    /// — a file `ab` holding `c`, and a file `a` holding `bc` — hash alike, and
    /// the second would be reported as a plateau it is not.
    #[test]
    fn a_boundary_between_fields_cannot_be_moved() {
        assert_ne!(digest_of(&vec![f("ab", "c")]), digest_of(&vec![f("a", "bc")]));
    }

    /// Order matters: two files swapped are a different tree, and a candidate
    /// that only reordered its answer is still a new answer.
    #[test]
    fn order_is_part_of_the_candidate() {
        assert_ne!(
            digest_of(&vec![f("a", "1"), f("b", "2")]),
            digest_of(&vec![f("b", "2"), f("a", "1")])
        );
    }

    /// Only what failed goes back to the model. A repair handed the passing
    /// checks spends its attention on the parts that are already fine — and those
    /// are exactly the ones it must not disturb.
    #[test]
    fn only_the_failing_checks_are_handed_back() {
        let got = failures_of(&[
            outcome("compiles", true, "ok"),
            outcome("the-fix", false, "expected 42, found 41"),
            outcome("lints", true, ""),
        ]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "the-fix");
        assert_eq!(got[0].detail, "expected 42, found 41", "the detail is what makes it actionable");
    }

    fn goal(context: &[(&str, &str)]) -> Goal {
        Goal {
            text: "make it 42".into(),
            context: context.iter().map(|(p, c)| f(p, c)).collect(),
            writable: vec!["src/lib.rs".into(), "src/new.rs".into()],
        }
    }

    /// THE REPAIR. Without this the model is shown the untouched tree on every
    /// attempt and can only rewrite from scratch, which makes "do not start over"
    /// a request it cannot honour.
    #[test]
    fn a_repair_is_shown_the_candidate_and_not_the_base() {
        let g = goal(&[("src/lib.rs", "fn answer() { 41 }")]);
        let best = vec![f("src/lib.rs", "fn answer() { 43 }")];

        assert_eq!(goal_for(&g, None).context[0].content, "fn answer() { 41 }", "the first attempt sees the base");
        let repair = goal_for(&g, Some(&best));
        assert_eq!(repair.context.len(), 1, "the candidate replaces the file, it does not duplicate it");
        assert_eq!(repair.context[0].content, "fn answer() { 43 }");
    }

    /// A file the candidate created and the goal never mentioned has to survive
    /// into the next prompt, or the loop creates it again — the same new file,
    /// three attempts running.
    #[test]
    fn a_file_the_candidate_invented_is_carried_forward() {
        let g = goal(&[("src/lib.rs", "old")]);
        let best = vec![f("src/new.rs", "invented")];
        let repair = goal_for(&g, Some(&best));
        assert_eq!(repair.context.len(), 2);
        assert_eq!(repair.context[1].path, "src/new.rs");
        assert_eq!(repair.context[0].content, "old", "and the untouched file is untouched");
    }

    /// The allow-list is the goal's, always. A candidate cannot widen what the
    /// next attempt may write by having written somewhere.
    #[test]
    fn the_candidate_cannot_widen_the_allow_list() {
        let g = goal(&[]);
        let repair = goal_for(&g, Some(&vec![f("src/lib.rs", "x")]));
        assert_eq!(repair.writable, g.writable);
    }

    #[test]
    fn a_clean_report_asks_for_no_repair() {
        assert!(failures_of(&[outcome("a", true, ""), outcome("b", true, "")]).is_empty());
    }
}
