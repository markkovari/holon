//! What a run reads from the knowledge pool, and what it writes back.
//!
//! `memory.rs` is the CLIENT — how to reach the pool and what the wire shapes
//! are. This is the POLICY: which of those calls a run makes, in what order, and
//! what it does when the pool cannot answer.
//!
//! One asymmetry runs through all of it and is the reason this is a file rather
//! than a handful of inline blocks (ADR-0084): **an error means DO THE WORK.**
//! An unreachable pool answers "not done", never "probably done". Redoing work
//! costs money; skipping work that was never done is a silent wrong answer, and
//! the two are not the same kind of mistake. Every function here fails in that
//! direction, and keeping them together is what makes that checkable.

use serde_json::{json, Value};

use comp_reconciler::memory::{self, Memory};

use std::time::Duration;

use comp_reconciler::contract::Answerer;
use comp_reconciler::fleet::repo_root;
use comp_reconciler::generation as generation_mod;
use comp_reconciler::generation::Entry;
use comp_reconciler::trace::Trace;

use crate::{Args, GoalSpec};
/// Has this goal already been done?
///
/// Asked ONCE per goal, before anything is spawned — the call that saves a whole
/// generation. Its failure mode had to be decided rather than defaulted: an
/// unreachable pool answers "no", because redoing work costs money and skipping
/// work that was never done is a silent wrong answer.
///
/// `false` means stop.
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

/// "Do we already have something for this?" — asked of the pool, before a single
/// token is spent, whatever the answer turns out to be.
///
/// ADR-0089 made reuse ENFORCED (a gate fails a part that reimplements
/// `auth-guard`) but never DISCOVERED: a human wrote the interfaces into the
/// goal's WIT and the branch then had no choice. That does not compound — every
/// new goal needed somebody who already knew what 150 components contained.
///
/// Mandatory rather than advisory because the ANSWER is the point in both
/// directions. A hit is reuse a branch would otherwise have missed. A miss is the
/// graph naming a capability the pool lacks — the only corpus in this system that
/// answers "what should we build next" — and it accumulates only if the question
/// is asked on every run, including the ones where nobody expected an answer.
///
/// No model, and nothing is blocked on the result: one millisecond of term overlap
/// over the catalogue, and a run whose search found nothing proceeds, with a row
/// recorded saying so (ADR-0094).
pub fn search_the_pool(
    goal_text: &str,
    run: &str,
    trace: Option<&Trace>,
) -> Vec<comp_reconciler::capsearch::Capability> {
    let catalog =
        comp_reconciler::plug::Catalog::scan(&comp_reconciler::plug::default_dirs(&repo_root()));
    let mut apps_of: std::collections::BTreeMap<String, usize> = Default::default();
    for name in catalog.names().map(String::from).collect::<Vec<_>>() {
        for part in catalog.closure(&name) {
            *apps_of.entry(part).or_default() += 1;
        }
    }
    let pool = comp_reconciler::capsearch::capabilities(&repo_root(), &catalog, &apps_of);
    let hits = comp_reconciler::capsearch::find(goal_text, &pool);
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
    hits.into_iter().take(5).map(|m| m.capability.clone()).collect()
}

/// Context content as a part should see it, trimmed for a small window.
///
/// A `.wit` shown as context is 68–79% comment (measured across this repository's interfaces),
/// and every load-bearing fact those comments carry is already in the contract — that is the
/// "KEPT" discipline the goals are written to. So the comments are redundant for a part and pure
/// cost for its context window: stripping them takes a WIT from ~700 tokens to ~200 and lets a
/// self-hosted model spend its window on the signatures it will call rather than on prose it can
/// read in the canonical file.
///
/// Only `.wit`, and only read-only context — a part's own writable `.rs` stub keeps every
/// comment, because there the comments ARE the brief. The canonical files are never touched;
/// this transforms the copy that goes into the prompt.
pub fn lean_context(path: &str, content: String) -> String {
    if !path.ends_with(".wit") {
        return content;
    }
    content.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// What the pool already has, as prose in every branch's context.
///
/// Prose rather than an instruction: the gate decides whether reuse happened, and
/// a branch TOLD to reuse something that does not fit would do it badly.
pub fn pool_context(reuse: &[comp_reconciler::capsearch::Capability]) -> Option<Value> {
    if reuse.is_empty() {
        return None;
    }
    let listed = reuse
        .iter()
        .map(|c| {
            format!(
                "- `{}` (in {} app(s)) exports {} — {}",
                c.name,
                c.apps,
                c.exports.iter().cloned().collect::<Vec<_>>().join(", "),
                c.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(json!({
        "path": "POOL.md",
        "content": format!(
            "# Capabilities this repository already has\n\nSearched for this goal. Composing \
             one of these is cheaper than writing it, and the gate reads what a candidate \
             actually called.\n\n{listed}\n"
        ),
    }))
}

/// Distil what the winner taught, and keep the pool bounded.
///
/// An agent may record what it OBSERVED; only a passing gate may promote
/// (ADR-0084). So this runs after the verdict, through the `promotion` interface an
/// agent's world does not contain — and it costs one cheap call that most
/// candidates answer with NOTHING, which is the correct answer for a candidate
/// that taught nobody anything.
///
/// The sweep is last on purpose: one that ran first could delete a lesson this run
/// was about to read.
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
/// The single-part path promotes through `promote_and_sweep`; the decomposed path never did,
/// which is why — measured across twelve runs of this experiment — the pool held only `errors`
/// rows and not one promotion, even from runs that opened a pull request. A perfect run taught
/// the graph nothing.
///
/// Each part is promoted keyed on the PART's own text, exactly as `compose.rs` RECALLS it: a
/// lesson keyed on the whole-goal wording would be invisible to the next part that recalls on
/// its own. Only a part whose gate accepted is promoted (ADR-0084), and the distiller answers
/// most of them with nothing, which is the right answer for a part that taught nobody anything.
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
        // The part's own text is the key recall uses; the part name is its env.
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
