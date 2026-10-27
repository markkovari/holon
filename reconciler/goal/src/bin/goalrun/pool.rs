//! Pool policy. ADR-0084: an error means do the work; an unreachable pool answers "not done".

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};

use holon_goal::memory::{self, Memory};

use holon_fleet::fleet::repo_root;
use holon_goal::contract::Answerer;
use holon_goal::generation as generation_mod;
use holon_goal::generation::Entry;
use holon_goal::trace::Trace;

use crate::{Args, GoalSpec};
/// Asked once per goal, before anything spawns; an unreachable pool answers "not done".
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

/// Asked on every run, hit or miss, before a token is spent (ADR-0094). `advisor_url` adds one
/// batched Jev check per hit, as term overlap over-matches; absent or unreachable trusts every hit.
pub fn search_the_pool(
    goal_text: &str,
    run: &str,
    trace: Option<&Trace>,
    advisor_url: Option<&str>,
) -> (Vec<holon_goal::capsearch::Capability>, Option<BTreeSet<String>>) {
    let catalog = holon_fleet::plug::Catalog::scan(&holon_fleet::plug::default_dirs(&repo_root()));
    let mut apps_of: std::collections::BTreeMap<String, usize> = Default::default();
    for name in catalog.names().map(String::from).collect::<Vec<_>>() {
        for part in catalog.closure(&name) {
            *apps_of.entry(part).or_default() += 1;
        }
    }
    let pool = holon_goal::capsearch::capabilities(&repo_root(), &catalog, &apps_of);
    let hits = holon_goal::capsearch::find(goal_text, &pool);
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
    let reuse: Vec<holon_goal::capsearch::Capability> =
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

/// `None` on any failure: the caller trusts every hit and never blocks on this.
fn confirm_with_advisor(
    advisor_url: &str,
    goal_text: &str,
    hits: &[holon_goal::capsearch::Capability],
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

/// Strips comments from read-only `.wit` context (68-79% comment) for a small window; writable
/// files keep theirs, since there the comments are the brief.
pub fn lean_context(path: &str, content: String) -> String {
    if !path.ends_with(".wit") {
        return content;
    }
    content.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// Prose, not an instruction: the gate decides whether reuse happened. `confirmed` splits what Jev
/// thinks fits from what it does not.
pub fn pool_context(
    reuse: &[holon_goal::capsearch::Capability],
    confirmed: Option<&BTreeSet<String>>,
) -> Option<Value> {
    if reuse.is_empty() {
        return None;
    }
    let line = |c: &holon_goal::capsearch::Capability| {
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

/// Only a passing gate may promote (ADR-0084). The sweep is last: first, it could delete a lesson
/// this run reads.
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

/// Keyed on the part's own text, as `compose.rs` recalls it; only accepted parts (ADR-0084).
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

/// Varied across branches on purpose (ADR-0081's herding); one control branch reads nothing.
pub(crate) fn reading_per_branch(
    args: &Args,
    goal: &GoalSpec,
    memory: Option<&Memory>,
) -> (Vec<generation_mod::Strategy>, Vec<Vec<String>>) {
    let mut strategies = generation_mod::default_strategies(args.branches);
    let mut read_by_branch: Vec<Vec<String>> = vec![Vec::new(); strategies.len()];
    // Tags make what it learns findable by the next goal on the same interfaces (ADR-0090).
    let tags = holon_fleet::plug::tags_for(
        &goal.writable,
        &holon_fleet::plug::Catalog::scan(&holon_fleet::plug::default_dirs(&repo_root())),
    );
    if let Some(m) = &memory {
        for (i, s) in strategies.iter_mut().enumerate() {
            if !s.reads_prior {
                continue; // the control arm reads nothing, and that is the point
            }
            let reading = memory::Reading {
                // Unequal so two branches do not reach the same prompt via the same advice.
                k: 3 + (i as u32 % 3),
                budget: 1200,
                tags: tags.clone(),
                min_similarity: 0.0,
                pools: match i % 3 {
                    0 => vec![], // everything
                    1 => vec!["errors".into()],
                    _ => vec!["patterns".into(), "solutions".into()],
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
        // Herding is reading the same thing, not nothing; an empty pool is just a first run.
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
