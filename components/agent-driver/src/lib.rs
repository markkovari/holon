#[allow(warnings)]
mod bindings;

use bindings::exports::graph::run::driver::{
    Attempt, Failure, File, Goal, Guest, Plan, RunError, RunResult, StopReason,
};
use bindings::graph::agent::writer as agent;
use bindings::graph::fitness::evaluator as gate;

use sha2::{Digest, Sha256};

struct Component;

/// Length-prefixed per field so `("ab", "c")` and `("a", "bc")` do not hash alike and fake a plateau.
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

/// Drops passing checks (a repair must not disturb them) and never-attempted ones (ADR-0088).
fn failures_of(outcomes: &[gate::Outcome]) -> Vec<Failure> {
    outcomes
        .iter()
        .filter(|o| o.state == gate::CheckState::Failed)
        .map(|o| Failure { id: o.id.clone(), detail: o.detail.clone() })
        .collect()
}

fn blocked_by(outcomes: &[gate::Outcome]) -> Vec<agent::Blocked> {
    outcomes
        .iter()
        .filter(|o| o.state == gate::CheckState::NotAttempted)
        .map(|o| agent::Blocked { id: o.id.clone(), needs: o.blocked_by.clone() })
        .collect()
}

/// Explicit rather than a shared type: the two contracts' files mean different things.
fn as_gate_files(files: &[File]) -> Vec<gate::File> {
    files.iter().map(|f| gate::File { path: f.path.clone(), content: f.content.clone() }).collect()
}

/// A file the candidate created that the goal never mentioned is appended, or the next attempt recreates it.
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
            // The evaluator refuses this too; refused here before paying for inference.
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
                // One level: a caller wanting ordering declares it in the goal spec.
                needs: Vec::new(),
            })
            .collect();

        let mut attempts: Vec<Attempt> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        // Seeded from the plan so a generation inherits what the last one could not fix.
        let mut previous: Vec<agent::Failure> = p
            .previous
            .iter()
            .map(|f| agent::Failure { id: f.id.clone(), detail: f.detail.clone() })
            .collect();
        let mut blocked: Vec<agent::Blocked> = Vec::new();
        let mut best: Option<(u32, Vec<File>, Vec<Failure>)> = None;
        let mut spent: u32 = 0;
        let mut stale: u32 = 0;
        // The runner caches base trees by commit: sent once, again only if it says it forgot.
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

            let goal = goal_for(&p.goal, best.as_ref().map(|(_, f, _)| f));

            let produced = match agent::attempt(&goal, &previous, &blocked, seed) {
                Ok(c) => c,
                // Another seed may fix it, so this costs an attempt, not the run.
                Err(agent::AgentError::UnusableAnswer(m)) => {
                    // Tell the next attempt, or the same prompt resamples the same format mistake.
                    previous =
                        vec![agent::Failure { id: "answer-format".into(), detail: m.clone() }];
                    // No candidate means no reported cost; `max-attempts` bounds what this burned.
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
                // Every remaining attempt would fail the same way.
                Err(agent::AgentError::InferenceFailed(m)) => {
                    return Err(RunError::ProviderDown(m))
                }
                // No seed fixes a wrong goal.
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

            // Bounded at two tries: a runner asking for a base it was just handed is broken.
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
            // Replaced, not accumulated: a check blocked last time that ran this time is no longer blocked.
            blocked = blocked_by(&v.outcomes);
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

            // Strictly better, so the first attempt to reach a score keeps the slot.
            if best.as_ref().is_none_or(|(s, _, _)| v.score > *s) {
                best = Some((v.score, files, failures.clone()));
                stale = 0;
            } else {
                stale += 1;
            }

            if v.accepted {
                return Ok(finish(StopReason::Accepted, best, attempts, spent));
            }

            // From the best candidate, since that is what the next attempt is shown.
            previous = best
                .as_ref()
                .map(|(_, _, f)| f.clone())
                .unwrap_or(failures)
                .iter()
                .map(|f| agent::Failure { id: f.id.clone(), detail: f.detail.clone() })
                .collect();

            // Checked after the attempt (cost is unknown until paid), so a run overshoots by at most one.
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
            state: if passed { gate::CheckState::Passed } else { gate::CheckState::Failed },
            blocked_by: String::new(),
            took_ms: 0,
            detail: detail.into(),
        }
    }

    #[test]
    fn a_blocked_check_is_context_and_never_the_subject_of_a_repair() {
        let outcomes = vec![
            outcome("compiles", false, "expected `;`"),
            gate::Outcome {
                id: "tests".into(),
                required: true,
                weight: 1,
                state: gate::CheckState::NotAttempted,
                blocked_by: "compiles".into(),
                took_ms: 0,
                detail: String::new(),
            },
        ];
        let failures = failures_of(&outcomes);
        assert_eq!(failures.len(), 1, "only the compile is a failure: {failures:?}");
        assert_eq!(failures[0].id, "compiles");
        let blocked = blocked_by(&outcomes);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].id, "tests");
        assert_eq!(blocked[0].needs, "compiles", "the check it is waiting on, unformatted");
    }

    #[test]
    fn the_same_candidate_digests_the_same_and_a_different_one_does_not() {
        let a = vec![f("src/lib.rs", "42")];
        assert_eq!(digest_of(&a), digest_of(&vec![f("src/lib.rs", "42")]));
        assert_ne!(digest_of(&a), digest_of(&vec![f("src/lib.rs", "41")]));
        assert_ne!(digest_of(&a), digest_of(&vec![f("src/other.rs", "42")]));
    }

    #[test]
    fn a_boundary_between_fields_cannot_be_moved() {
        assert_ne!(digest_of(&vec![f("ab", "c")]), digest_of(&vec![f("a", "bc")]));
    }

    #[test]
    fn order_is_part_of_the_candidate() {
        assert_ne!(
            digest_of(&vec![f("a", "1"), f("b", "2")]),
            digest_of(&vec![f("b", "2"), f("a", "1")])
        );
    }

    #[test]
    fn only_the_failing_checks_are_handed_back() {
        let got = failures_of(&[
            outcome("compiles", true, "ok"),
            outcome("the-fix", false, "expected 42, found 41"),
            outcome("lints", true, ""),
        ]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "the-fix");
        assert_eq!(
            got[0].detail, "expected 42, found 41",
            "the detail is what makes it actionable"
        );
    }

    fn goal(context: &[(&str, &str)]) -> Goal {
        Goal {
            text: "make it 42".into(),
            context: context.iter().map(|(p, c)| f(p, c)).collect(),
            writable: vec!["src/lib.rs".into(), "src/new.rs".into()],
        }
    }

    #[test]
    fn a_repair_is_shown_the_candidate_and_not_the_base() {
        let g = goal(&[("src/lib.rs", "fn answer() { 41 }")]);
        let best = vec![f("src/lib.rs", "fn answer() { 43 }")];

        assert_eq!(
            goal_for(&g, None).context[0].content,
            "fn answer() { 41 }",
            "the first attempt sees the base"
        );
        let repair = goal_for(&g, Some(&best));
        assert_eq!(
            repair.context.len(),
            1,
            "the candidate replaces the file, it does not duplicate it"
        );
        assert_eq!(repair.context[0].content, "fn answer() { 43 }");
    }

    #[test]
    fn a_file_the_candidate_invented_is_carried_forward() {
        let g = goal(&[("src/lib.rs", "old")]);
        let best = vec![f("src/new.rs", "invented")];
        let repair = goal_for(&g, Some(&best));
        assert_eq!(repair.context.len(), 2);
        assert_eq!(repair.context[1].path, "src/new.rs");
        assert_eq!(repair.context[0].content, "old", "and the untouched file is untouched");
    }

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
