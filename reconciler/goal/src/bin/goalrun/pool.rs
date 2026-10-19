//! What a run reads from the knowledge pool, and what it writes back.
//!
//! `memory.rs` is the CLIENT (how to reach the pool, wire shapes). This is the POLICY: which calls a
//! run makes, in what order, and what it does when the pool cannot answer.
//!
//! One asymmetry runs through all of it (ADR-0084): **an error means DO THE WORK.** An unreachable
//! pool answers "not done", never "probably done", since skipping work never done is a silent wrong
//! answer. Every function here fails in that direction.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};

use comp_goal::memory::{self, Memory};

use comp_fleet::fleet::repo_root;
use comp_goal::contract::Answerer;
use comp_goal::generation as generation_mod;
use comp_goal::generation::Entry;
use comp_goal::trace::Trace;

use crate::{Args, GoalSpec};
/// Has this goal already been done?
///
/// Asked ONCE per goal, before anything is spawned: the call that saves a whole generation. An
/// unreachable pool answers "no". `false` means stop.
pub fn worth_running(memory: Option<&Memory>, goal: &GoalSpec, skip_above: f64) -> bool {
    let Some(m) = memory else { return true };
    match m.already_done(&goal.text, skip_above) {
        Ok(Some(prior)) => {
            println!("\nALREADY DONE — {}", prior.summary());
            println!(
                "\n  no branches spawned. Lower --skip-above (now {skip_above:.2}) or clear the \
                 pool if this is not the same work."
            );
            false
        }
        Ok(None) => {
            println!("nothing on record for this goal; running it");
            true
        }
        Err(e) => {
            println!("could not ask the knowledge pool ({e}) — doing the work");
            true
        }
    }
}

/// "Do we already have something for this?" Asked of the pool before a single token is spent,
/// whatever the answer.
/// ADR-0089 made reuse ENFORCED but never DISCOVERED: a human wrote the interfaces into the goal's WIT
/// and the branch had no choice, which does not compound.
///
/// Mandatory because the ANSWER is the point in both directions: a hit is reuse a branch would have
/// missed; a miss is the graph naming a capability the pool lacks, which accumulates only if the
/// question is asked on every run. No model, nothing blocked: a run whose search found nothing
/// proceeds, with a row recorded saying so (ADR-0094).
///
/// `advisor_url`, given, adds ONE step: term overlap over-matches (many descriptions are tautological),
/// so each hit gets one Jev question, batched into a single call to `capability-advisor`. `None`, or
/// an unreachable advisor, trusts every hit as before.
pub fn search_the_pool(
    goal_text: &str,
    run: &str,
    trace: Option<&Trace>,
    advisor_url: Option<&str>,
) -> (Vec<comp_goal::capsearch::Capability>, Option<BTreeSet<String>>) {
    let catalog = comp_fleet::plug::Catalog::scan(&comp_fleet::plug::default_dirs(&repo_root()));
    let mut apps_of: std::collections::BTreeMap<String, usize> = Default::default();
    for name in catalog.names().map(String::from).collect::<Vec<_>>() {
        for part in catalog.closure(&name) {
            *apps_of.entry(part).or_default() += 1;
        }
    }
    let pool = comp_goal::capsearch::capabilities(&repo_root(), &catalog, &apps_of);
    let hits = comp_goal::capsearch::find(goal_text, &pool);
    if let Some(t) = trace {
        t.capsearch(run, goal_text, hits.len());
    }
    if hits.is_empty() {
        println!(
            "capability search: nothing in the pool answers this goal — if the work \
             generalises, it is a candidate for promotion (ADR-0089)\n"
        );
    } else {
        println!("capability search: {} candidate(s) the pool already has:", hits.len());
        for m in hits.iter().take(5) {
            println!(
                "  {:<22} {} app(s)  {}",
                m.capability.name,
                m.capability.apps,
                m.capability.description.chars().take(88).collect::<String>()
            );
        }
        println!();
    }
    let reuse: Vec<comp_goal::capsearch::Capability> =
        hits.into_iter().take(5).map(|m| m.capability.clone()).collect();

    let confirmed = advisor_url.filter(|_| !reuse.is_empty()).and_then(|url| {
        match confirm_with_advisor(url, goal_text, &reuse) {
            Some(c) => {
                let rejected = reuse.len() - c.len();
                println!(
                    "capability advisor: {} confirmed, {} not — see POOL.md\n",
                    c.len(),
                    rejected
                );
                if let Some(t) = trace {
                    t.capsearch_confirmed(run, c.len(), rejected);
                }
                Some(c)
            }
            None => {
                println!(
                    "capability advisor: unavailable — trusting the search above unfiltered\n"
                );
                None
            }
        }
    });

    (reuse, confirmed)
}

/// One HTTP call to `capability-advisor`, batching one Jev question per hit. `None` on ANY failure:
/// the caller trusts every hit unfiltered and never blocks on this.
fn confirm_with_advisor(
    advisor_url: &str,
    goal_text: &str,
    hits: &[comp_goal::capsearch::Capability],
) -> Option<BTreeSet<String>> {
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().ok()?;
    let candidates: Vec<Value> = hits
        .iter()
        .enumerate()
        .map(|(i, c)| json!({"id": i.to_string(), "name": c.name, "description": c.description}))
        .collect();
    let resp = http
        .post(format!("{}/evaluate", advisor_url.trim_end_matches('/')))
        .json(&json!({"goal": goal_text, "candidates": candidates}))
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().ok()?;
    if body["unavailable"].as_bool().unwrap_or(true) {
        return None;
    }
    Some(
        body["confirmed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["name"].as_str().map(str::to_string))
            .collect(),
    )
}

/// Context content as a part should see it, trimmed for a small window.
///
/// A `.wit` shown as context is 68–79% comment (measured), and every load-bearing fact is already in
/// the contract, so stripping the comments (~700 tokens to ~200) lets a self-hosted model spend its
/// window on signatures. Only `.wit`, and only read-only context: a part's own writable `.rs` stub
/// keeps every comment, because there the comments ARE the brief. Canonical files are never touched;
/// this transforms the prompt copy.
pub fn lean_context(path: &str, content: String) -> String {
    if !path.ends_with(".wit") {
        return content;
    }
    content.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// What the pool already has, as prose in every branch's context.
///
/// Prose rather than an instruction: the gate decides whether reuse happened, and a branch TOLD to
/// reuse something that does not fit would do it badly.
/// `confirmed`, given, splits the listing into what Jev thinks fits and what does not, since term
/// overlap alone over-matches. `None` (no advisor, or unreachable) lists everything.
pub fn pool_context(
    reuse: &[comp_goal::capsearch::Capability],
    confirmed: Option<&BTreeSet<String>>,
) -> Option<Value> {
    if reuse.is_empty() {
        return None;
    }
    let line = |c: &comp_goal::capsearch::Capability| {
        format!(
            "- `{}` (in {} app(s)) exports {} — {}",
            c.name,
            c.apps,
            c.exports.to_vec().join(", "),
            c.description
        )
    };
    let content = match confirmed {
        None => format!(
            "# Capabilities this repository already has\n\nSearched for this goal. Composing \
             one of these is cheaper than writing it, and the gate reads what a candidate \
             actually called.\n\n{}\n",
            reuse.iter().map(line).collect::<Vec<_>>().join("\n")
        ),
        Some(confirmed) => {
            let (fits, maybe): (Vec<_>, Vec<_>) =
                reuse.iter().partition(|c| confirmed.contains(&c.name));
            let mut sections = String::from(
                "# Capabilities this repository already has\n\nSearched for this goal, then \
                 each one checked against it individually. Composing one of these is cheaper \
                 than writing it, and the gate reads what a candidate actually called.\n\n",
            );
            if !fits.is_empty() {
                sections.push_str("## Likely fits this goal\n\n");
                sections.push_str(&fits.iter().map(|c| line(c)).collect::<Vec<_>>().join("\n"));
                sections.push('\n');
            }
            if !maybe.is_empty() {
                sections.push_str(
                    "\n## Matched the search, but may not actually fit — check before using\n\n",
                );
                sections.push_str(&maybe.iter().map(|c| line(c)).collect::<Vec<_>>().join("\n"));
                sections.push('\n');
            }
            sections
        }
    };
    Some(json!({ "path": "POOL.md", "content": content }))
}

/// Distil what the winner taught, and keep the pool bounded.
///
/// An agent may record what it OBSERVED; only a passing gate may promote (ADR-0084). So this runs after
/// the verdict, through the `promotion` interface an agent's world does not contain, and costs one
/// cheap call that most candidates answer with NOTHING. The sweep is last on purpose: one that ran
/// first could delete a lesson this run was about to read.
pub fn promote_and_sweep(
    memory: Option<&Memory>,
    args: &Args,
    goal: &GoalSpec,
    port: u16,
    best: Option<&Entry>,
    winner_ref: &str,
) {
    let Some(m) = memory else { return };
    if let Some(best) = best.filter(|b| b.accepted) {
        let door = Answerer {
            url: format!("http://127.0.0.1:{port}"),
            host: "goalanswer.acme.test".into(),
            timeout: Duration::from_secs(180),
        };
        let prompt = memory::distil_prompt(&goal.text, &best.files, best.score);
        match door.reply_to(&prompt).map(|r| memory::distilled(&r)) {
            Ok(Some(lesson)) => {
                match m.promote(&goal.text, &best.branch, winner_ref, &lesson, best.score) {
                    Ok(h) => println!("\npromoted to patterns: {h}\n  {lesson}"),
                    Err(e) => println!("\n(nothing promoted: {e})"),
                }
            }
            Ok(None) => println!("\nthe winner taught nothing transferable, and said so"),
            Err(e) => println!("\n(the distiller could not be reached: {e})"),
        }
    }

    if args.forget_after_days > 0 {
        match m.decay(args.forget_after_days, 2) {
            Ok(0) => {}
            Ok(n) => println!(
                "knowledge: forgot {n} entr{} nothing had read in {} days",
                if n == 1 { "y" } else { "ies" },
                args.forget_after_days
            ),
            Err(e) => println!("knowledge: could not sweep the pool ({e})"),
        }
    }
}

/// Promote what each part's winner taught, on a composed run.
///
/// The single-part path promotes through `promote_and_sweep`; the decomposed path never did, so across
/// twelve runs the pool held only `errors` rows. Each part is promoted keyed on the PART's own text,
/// as `compose.rs` RECALLS it: a lesson keyed on the whole-goal wording would be invisible to the
/// next part. Only a part whose gate accepted is promoted (ADR-0084).
pub fn promote_parts(
    memory: Option<&Memory>,
    goal: &GoalSpec,
    port: u16,
    parts: &[generation_mod::PartOutcome],
) {
    let Some(m) = memory else { return };
    let door = Answerer {
        url: format!("http://127.0.0.1:{port}"),
        host: "goalanswer.acme.test".into(),
        timeout: Duration::from_secs(180),
    };
    for outcome in parts {
        let Some(best) = outcome.best.as_ref().filter(|b| b.accepted) else { continue };
        let Some(spec) = goal.parts.iter().find(|p| p.name == outcome.part) else { continue };
        let prompt = memory::distil_prompt(&spec.text, &best.files, best.score);
        match door.reply_to(&prompt).map(|r| memory::distilled(&r)) {
            Ok(Some(lesson)) => {
                match m.promote(&spec.text, &outcome.part, &best.branch, &lesson, best.score) {
                    Ok(h) => println!("  promoted {}: {h}\n    {lesson}", outcome.part),
                    Err(e) => println!("  {} promoted nothing: {e}", outcome.part),
                }
            }
            Ok(None) => println!("  {} taught nothing transferable, and said so", outcome.part),
            Err(e) => println!("  {} — the distiller could not be reached: {e}", outcome.part),
        }
    }
}

/// What each branch is allowed to read, and what it actually read.
///
/// Varied ACROSS branches on purpose: branches that all read the same top-k are an expensive way to
/// run one (ADR-0081's herding), and the branch that reads nothing is the only way to tell whether
/// the pool helps. `default_strategies` already keeps one branch from reading the previous winner;
/// that same branch reads no lessons either. Returns the strategies and, per branch, the keys it read
/// (the other half of attribution).
pub(crate) fn reading_per_branch(
    args: &Args,
    goal: &GoalSpec,
    memory: Option<&Memory>,
) -> (Vec<generation_mod::Strategy>, Vec<Vec<String>>) {
    let mut strategies = generation_mod::default_strategies(args.branches);
    let mut read_by_branch: Vec<Vec<String>> = vec![Vec::new(); strategies.len()];
    // What this goal's work touches, so what it learns is findable by the next goal that builds against
    // the same interfaces (ADR-0090).
    let tags = comp_fleet::plug::tags_for(
        &goal.writable,
        &comp_fleet::plug::Catalog::scan(&comp_fleet::plug::default_dirs(&repo_root())),
    );
    if let Some(m) = &memory {
        for (i, s) in strategies.iter_mut().enumerate() {
            if !s.reads_prior {
                continue; // the control arm reads nothing, and that is the point
            }
            let reading = memory::Reading {
                // Deliberately unequal (3, 4, 5 …) so two branches do not reach the same prompt via the same advice.
                k: 3 + (i as u32 % 3),
                budget: 1200,
                tags: tags.clone(),
                min_similarity: 0.0,
                pools: match i % 3 {
                    0 => vec![],                                      // everything
                    1 => vec!["errors".into()],                       // only what failed
                    _ => vec!["patterns".into(), "solutions".into()], // only what worked
                },
            };
            match m.recall(&goal.text, &reading) {
                Ok(lessons) if lessons.is_empty() => {}
                Ok(lessons) => {
                    println!(
                        "  branch-{i} reads {} lesson(s) [{}]",
                        lessons.len(),
                        lessons.iter().map(|l| l.ns.as_str()).collect::<Vec<_>>().join(", ")
                    );
                    read_by_branch[i] = lessons.iter().map(|l| l.key.clone()).collect();
                    s.knowledge = memory::render(&lessons);
                }
                Err(e) => println!("  branch-{i} runs cold: {e}"),
            }
        }
        // Herding is branches reading the SAME thing, not nothing: an empty pool makes every reading
        // identical, a cold start rather than convergence; warning on it would cry wolf on every first run.
        let readers = read_by_branch.iter().filter(|r| !r.is_empty()).count();
        if readers > 1 {
            let distinct: std::collections::BTreeSet<&Vec<String>> =
                read_by_branch.iter().filter(|r| !r.is_empty()).collect();
            println!(
                "  knowledge: {} distinct reading(s) across {readers} reading branches{}",
                distinct.len(),
                if distinct.len() == 1 {
                    " — every one read the same thing, which is herding"
                } else {
                    ""
                }
            );
        } else if readers == 0 {
            println!("  knowledge: the pool had nothing for this goal; every branch runs cold");
        }
    }
    (strategies, read_by_branch)
}
