//! The goal runner's client for `knowledge:memory` — skip work already done, and
//! record every verdict so the next run can.
//!
//! ## Nothing here may fail a run
//!
//! Every function answers with a plain value and swallows what went wrong into a `String` the caller
//! may print: a knowledge pool that is down, misconfigured or absent must cost a run nothing but the
//! knowledge. **An error means DO THE WORK**: `already_done` answers `None` when it cannot reach the
//! pool, never "probably done", since skipping work not done is a silent wrong answer and redoing it
//! only costs money (ADR-0084).
//!
//! ## Why HTTP and not a link
//!
//! The reconciler is native and reaches components through the ingress, as it reaches the driver and
//! selector: `memory-probe` in front of `knowledge-memory` is the shape of `driver-probe` in front of
//! `agent-driver`.

use std::time::Duration;

use serde_json::Value;

/// Where the memory app is, and the ingress host that routes to it.
///
/// `None` is the default: a run without `--surreal-url` deploys no memory app and makes no calls.
#[derive(Clone)]
pub struct Memory {
    pub url: String,
    pub host: String,
    pub timeout: Duration,
}

/// What a past passing run of this goal left behind.
pub struct Prior {
    pub goal: String,
    pub similarity: f64,
    pub score: i64,
    pub run: String,
    pub artifact: String,
    pub evaluations: u64,
}

impl Prior {
    /// One line for a human deciding whether the skip was right.
    pub fn summary(&self) -> String {
        let artifact = if self.artifact.is_empty() {
            "nothing addressable".to_string()
        } else {
            self.artifact.clone()
        };
        // One line of it: a goal is often a paragraph.
        let goal = self.goal.lines().next().unwrap_or_default();
        let goal = if goal.len() > 72 { format!("{}…", &goal[..71]) } else { goal.to_string() };
        format!(
            "\"{goal}\" (similarity {:.3}) already passed at score {} in run {} → {artifact}; \
             {} evaluation(s) on record",
            self.similarity, self.score, self.run, self.evaluations
        )
    }
}

fn client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(timeout).build().expect("http client")
}

fn enc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            other => other.to_string().bytes().map(|b| format!("%{b:02X}")).collect::<String>(),
        })
        .collect()
}

impl Memory {
    fn call(&self, method: reqwest::Method, path: &str) -> Result<Value, String> {
        self.call_with(method, path, String::new())
    }

    /// The same, with a body: `/observe` needs one because a lesson is prose.
    fn call_with(
        &self,
        method: reqwest::Method,
        path: &str,
        body: String,
    ) -> Result<Value, String> {
        let r = client(self.timeout)
            .request(method, format!("{}{path}", self.url))
            .header("host", &self.host)
            .body(body)
            .send()
            .map_err(|e| format!("{e}"))?;
        let text = r.text().unwrap_or_default();
        serde_json::from_str(&text).map_err(|e| format!("unreadable answer ({e}): {text}"))
    }

    /// Has this goal already been done? `Ok(None)` means go and do the work.
    ///
    /// Called ONCE per goal, before anything is spawned, not per branch.
    pub fn already_done(&self, goal: &str, min_similarity: f64) -> Result<Option<Prior>, String> {
        let v = self.call(
            reqwest::Method::GET,
            &format!("/already-done?goal={}&min={min_similarity}", enc(goal)),
        )?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        if v["found"] != Value::Bool(true) {
            return Ok(None);
        }
        Ok(Some(Prior {
            goal: v["goal"].as_str().unwrap_or_default().to_string(),
            similarity: v["similarity"].as_f64().unwrap_or(0.0),
            score: v["score"].as_i64().unwrap_or(0),
            run: v["run"].as_str().unwrap_or_default().to_string(),
            artifact: v["artifact"].as_str().unwrap_or_default().to_string(),
            evaluations: v["evaluations"].as_u64().unwrap_or(0),
        }))
    }

    /// Forget what nobody read. Returns how many entries went.
    ///
    /// Driven by the run, not a daemon: a `decay` that is exposed and never called is the gap ADR-0081
    /// caught in alpha-swarm2, and a few milliseconds per run beats a scheduler nobody deploys.
    pub fn decay(&self, days: u32, min_uses: u64) -> Result<u32, String> {
        let v =
            self.call(reqwest::Method::POST, &format!("/decay?days={days}&min-uses={min_uses}"))?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        Ok(v["forgotten"].as_u64().unwrap_or(0) as u32)
    }

    /// Record one verdict.
    ///
    /// Called once per BRANCH, not per generation: the count of failed attempts says whether another
    /// generation is worth buying. Idempotent per `(goal, run)`, so the landing path may call it again
    /// with the pull request.
    /// Record that this goal was decomposed into that one.
    ///
    /// Idempotent per `(parent, child)`. Failure is REPORTED, never fatal: losing the edge costs the pool
    /// its memory of the decomposition, not the run its result.
    pub fn decomposed_into(
        &self,
        parent: &str,
        child: &str,
        ordinal: u32,
        why: &str,
    ) -> Result<(), String> {
        let v = self.call(
            reqwest::Method::POST,
            &format!(
                "/decomposed-into?parent={}&child={}&ordinal={ordinal}&why={}",
                enc(parent),
                enc(child),
                enc(why)
            ),
        )?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        Ok(())
    }

    pub fn parts_of(&self, goal: &str) -> Result<Vec<SubGoal>, String> {
        self.sub_goals("/parts-of", goal)
    }

    /// The goals this one is a part of: plural, since one sub-goal reached from two parents is one node
    /// with two edges.
    pub fn parents_of(&self, goal: &str) -> Result<Vec<SubGoal>, String> {
        self.sub_goals("/parents-of", goal)
    }

    fn sub_goals(&self, route: &str, goal: &str) -> Result<Vec<SubGoal>, String> {
        let v = self.call(reqwest::Method::GET, &format!("{route}?goal={}", enc(goal)))?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        Ok(v["parts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|p| SubGoal {
                goal: p["goal"].as_str().unwrap_or_default().to_string(),
                ordinal: p["ordinal"].as_u64().unwrap_or(0) as u32,
                why: p["why"].as_str().unwrap_or_default().to_string(),
                done: p["done"].as_bool().unwrap_or(false),
            })
            .collect())
    }

    pub fn evaluated(
        &self,
        goal: &str,
        run: &str,
        score: u64,
        passed: bool,
        artifact: &str,
    ) -> Result<(), String> {
        let v = self.call(
            reqwest::Method::POST,
            &format!(
                "/evaluated?goal={}&run={}&score={score}&passed={passed}&artifact={}",
                enc(goal),
                enc(run),
                enc(artifact)
            ),
        )?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        Ok(())
    }
}

pub struct SubGoal {
    pub goal: String,
    pub ordinal: u32,
    pub why: String,
    /// Whether any run has PASSED on this part, derived from its verdict edges so it cannot disagree with
    /// the trail; what makes a parent resumable rather than all-or-nothing.
    pub done: bool,
}

pub struct Lesson {
    pub key: String,
    pub ns: String,
    pub text: String,
    pub dense: bool,
}

/// The DIVERSITY BUDGET, per branch because branches that all read the same top-k are an expensive way
/// to run one (ADR-0081). `k = 0` reads nothing: the control arm.
pub struct Reading {
    pub k: u32,
    pub budget: u32,
    pub pools: Vec<String>,
    /// Drop dense hits below this cosine. 0 disables it.
    ///
    /// A threshold asks for structural evidence WITHOUT whatever the embedding thinks of your goal; a tag
    /// match is exempt by design (ADR-0090).
    pub min_similarity: f64,
    /// Interfaces the part imports, so retrieval can cross applications.
    ///
    /// Text similarity finds a lesson when the wording rhymes; these find it when the CONTRACT is shared,
    /// which is what repeats: a fact about `csv:codec/codec` holds for a ledger and a clinic alike
    /// (ADR-0090). Empty is text-only, what the control arm wants.
    pub tags: Vec<String>,
}

impl Memory {
    /// What the swarm learned that bears on this goal.
    ///
    /// An error is NOT fatal and NOT an empty answer dressed up as one: the caller prints it and runs the
    /// branch cold.
    pub fn recall(&self, goal: &str, r: &Reading) -> Result<Vec<Lesson>, String> {
        if r.k == 0 {
            return Ok(Vec::new());
        }
        let v = self.call(
            reqwest::Method::GET,
            &format!(
                "/recall?goal={}&k={}&budget={}&pools={}&tags={}&min={}",
                enc(goal),
                r.k,
                r.budget,
                enc(&r.pools.join(",")),
                enc(&r.tags.join(",")),
                r.min_similarity
            ),
        )?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        Ok(v["hits"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|h| Lesson {
                key: h["key"].as_str().unwrap_or_default().to_string(),
                ns: h["ns"].as_str().unwrap_or_default().to_string(),
                text: h["text"].as_str().unwrap_or_default().to_string(),
                dense: h["dense"].as_bool().unwrap_or(false),
            })
            .collect())
    }

    /// What happened to the lessons a branch read: the ONLY thing that moves an entry's standing.
    pub fn attribute(&self, keys: &[String], run: &str, succeeded: bool) -> Result<(), String> {
        if keys.is_empty() {
            return Ok(());
        }
        let v = self.call(
            reqwest::Method::POST,
            &format!("/attribute?keys={}&run={}&ok={succeeded}", enc(&keys.join(",")), enc(run)),
        )?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        Ok(())
    }
}

/// What a branch learned by failing, in the gate's own words.
///
/// Deliberately not a model: the failing check ids and the first line of what the runner said.
/// Negative knowledge from a real verdict cannot be a hallucination, so it is visible to siblings
/// immediately (ADR-0081's asymmetry); its worst case is avoiding something that would have worked.
///
/// `None` when there is nothing to learn: a branch that passed, or one that never produced a candidate
/// and so failed for a reason about the harness.
pub fn failure_text(failures: &Value, score: u64) -> Option<String> {
    let list = failures.as_array()?;
    if list.is_empty() {
        return None;
    }
    let named: Vec<String> = list
        .iter()
        .filter_map(|f| {
            let id = f["id"].as_str()?;
            let first = f["detail"]
                .as_str()
                .unwrap_or("")
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            Some(if first.is_empty() {
                format!("`{id}` failed")
            } else {
                format!("`{id}` failed: {}", first.chars().take(200).collect::<String>())
            })
        })
        .collect();
    if named.is_empty() {
        return None;
    }
    Some(format!("scored {score}; {}", named.join("; ")))
}

impl Memory {
    pub fn observe_failure(
        &self,
        goal: &str,
        env: &str,
        attempt: &str,
        text: &str,
    ) -> Result<String, String> {
        self.observe_failure_tagged(goal, env, attempt, text, &[])
    }

    pub fn observe_failure_tagged(
        &self,
        goal: &str,
        env: &str,
        attempt: &str,
        text: &str,
        tags: &[String],
    ) -> Result<String, String> {
        let v = self.call_with(
            reqwest::Method::POST,
            &format!(
                "/observe?ns=errors&goal={}&env={}&attempt={}&tags={}",
                enc(goal),
                enc(env),
                enc(attempt),
                enc(&tags.join(","))
            ),
            text.to_string(),
        )?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        v["handle"].as_str().map(str::to_string).ok_or_else(|| format!("no handle in {v}"))
    }
}

/// What to ask a cheap model when a candidate has passed.
///
/// ONE paragraph of transferable advice, not a summary of the diff (which the diff already says). The
/// ≤900 characters is in the prompt as well as enforced after: a model told the limit writes to it,
/// one told nothing writes an essay that gets cut mid-sentence.
pub fn distil_prompt(goal: &str, files: &Value, score: u64) -> String {
    let changed: Vec<String> = files
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|f| {
            let path = f["path"].as_str()?;
            let body = f["content"].as_str().unwrap_or("");
            Some(format!("=== {path}\n{}", body.chars().take(1500).collect::<String>()))
        })
        .collect();
    format!(
        "A candidate just passed every check for this goal, scoring {score}.\n\n\
GOAL:\n{goal}\n\n\
WHAT PASSED:\n{}\n\n\
Write ONE paragraph, at most 900 characters, that a different agent attempting a \
SIMILAR goal in this codebase would be glad to have been told before it started.\n\n\
Transferable, not descriptive: name the trap, the invariant, the API that is not \
what it looks like, the thing the tests actually demanded. Do NOT summarise the \
diff — a future reader can read the diff. If there is genuinely nothing \
transferable here, answer exactly NOTHING and nothing else.",
        changed.join("\n\n")
    )
}

/// The lesson in a reply, or `None` when the model said there was none.
///
/// `NOTHING` is offered because most passing candidates teach nothing, and a pool that records a
/// platitude for each buries the few that matter.
pub fn distilled(reply: &str) -> Option<String> {
    let text = reply.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("nothing") {
        return None;
    }
    // A model that pasted the diff back has not distilled anything, and storing it would put a diff in
    // every future prompt.
    if text.contains("=== ") {
        return None;
    }
    let cut: String = text.chars().take(900).collect();
    Some(cut.split_whitespace().collect::<Vec<_>>().join(" "))
}

impl Memory {
    /// Promote a distilled lesson to `patterns`, the trusted pool.
    ///
    /// The ONLY writer of it, through the registry's `promotion` interface rather than `memory`: an
    /// agent's world does not contain this call (ADR-0084), the anti-poisoning argument. A score that
    /// did not pass is refused on the other side.
    pub fn promote(
        &self,
        goal: &str,
        env: &str,
        attempt: &str,
        text: &str,
        gate_score: u64,
    ) -> Result<String, String> {
        let v = self.call_with(
            reqwest::Method::POST,
            &format!(
                "/promote?goal={}&env={}&attempt={}&score={gate_score}",
                enc(goal),
                enc(env),
                enc(attempt)
            ),
            text.to_string(),
        )?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        v["handle"].as_str().map(str::to_string).ok_or_else(|| format!("no handle in {v}"))
    }
}

/// Lessons as a branch sees them: one bullet each, negative knowledge marked.
///
/// The namespace is shown because the three are not worth the same: `errors` did not work, `patterns`
/// survived a gate; a model handed both undifferentiated weighs them equally.
pub fn render(lessons: &[Lesson]) -> String {
    lessons
        .iter()
        .map(|l| {
            let tag = match l.ns.as_str() {
                "errors" => "AVOID",
                "patterns" => "PROVEN",
                _ => "TRIED",
            };
            format!("- [{tag}] {}", l.text.replace('\n', " "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The run id a verdict is attributed to.
///
/// Stable for one branch of one search, distinct across them, and readable (`gen 1 branch risk-first`);
/// the seed is in it because that makes a branch replayable (ADR-0078).
pub fn run_id(search_seed: u64, round: usize, branch: &str) -> String {
    format!("{search_seed}/g{round}/{branch}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_run_id_is_stable_distinct_and_legible() {
        assert_eq!(run_id(1234, 0, "risk-first"), "1234/g0/risk-first");
        assert_ne!(run_id(1234, 0, "risk-first"), run_id(1234, 1, "risk-first"));
        assert_ne!(run_id(1234, 0, "risk-first"), run_id(1235, 0, "risk-first"));
    }

    #[test]
    fn a_goal_survives_being_put_in_a_query_string() {
        assert_eq!(enc("make a slug from a title"), "make+a+slug+from+a+title");
        // The characters that would otherwise end the parameter, or the path.
        assert_eq!(enc("a&b=c?d/e"), "a%26b%3Dc%3Fd%2Fe");
        assert_eq!(enc("café"), "caf%C3%A9");
    }

    fn lesson(ns: &str, text: &str) -> Lesson {
        Lesson { key: format!("{ns}:1"), ns: ns.into(), text: text.into(), dense: true }
    }

    #[test]
    fn the_distiller_asks_for_advice_and_not_a_summary() {
        let p = distil_prompt(
            "make paginate handle an offset past the end",
            &json!([{ "path": "src/lib.rs", "content": "pub fn paginate() {}" }]),
            1000,
        );
        assert!(p.contains("scoring 1000"), "{p}");
        assert!(p.contains("make paginate handle an offset past the end"));
        assert!(p.contains("=== src/lib.rs"), "the files that passed are in it");
        assert!(p.contains("at most 900 characters"), "a model told the limit writes to it");
        assert!(p.contains("Do NOT summarise the diff"), "{p}");
        assert!(p.contains("answer exactly NOTHING"), "silence has to be offerable");
    }

    #[test]
    fn most_passing_candidates_teach_nothing_and_that_is_allowed() {
        assert_eq!(distilled("NOTHING"), None);
        assert_eq!(distilled("  nothing  "), None);
        assert_eq!(distilled(""), None);
        assert_eq!(distilled("=== src/lib.rs\npub fn x() {}"), None);
    }

    #[test]
    fn a_distilled_lesson_is_one_paragraph_within_the_budget() {
        let long = "word ".repeat(400);
        let out = distilled(&long).expect("a real answer");
        assert!(out.chars().count() <= 900, "{}", out.chars().count());
        assert_eq!(out.lines().count(), 1, "a lesson is rendered into a bullet");
        let out = distilled("  the tests demand `has_more`\n  not a total count  ").unwrap();
        assert_eq!(out, "the tests demand `has_more` not a total count");
    }

    #[test]
    fn a_failure_becomes_a_lesson_in_the_gate_s_own_words() {
        let failures = json!([
            { "id": "pager-renders", "detail": "grep: no match\n\nexit status 1" },
            { "id": "component-tests", "detail": "" }
        ]);
        let text = failure_text(&failures, 400).expect("a failed branch has something to teach");
        assert!(text.starts_with("scored 400;"), "{text}");
        assert!(text.contains("`pager-renders` failed: grep: no match"), "{text}");
        assert!(
            text.contains("`component-tests` failed"),
            "a check with no detail still names itself"
        );
        // One line, because it is rendered into a bullet list.
        assert_eq!(text.lines().count(), 1, "{text:?}");
    }

    #[test]
    fn a_branch_with_nothing_to_teach_writes_nothing() {
        // A branch that passed, and one that never produced a candidate (a harness failure, not a code one).
        assert!(failure_text(&json!([]), 1000).is_none());
        assert!(failure_text(&json!(null), 0).is_none());
    }

    #[test]
    fn a_lesson_says_which_kind_it_is() {
        let out = render(&[
            lesson("errors", "a bare unwrap fails the gate"),
            lesson("patterns", "split on syntax, not token count"),
            lesson("solutions", "the retry belongs in the client"),
        ]);
        assert!(out.contains("[AVOID] a bare unwrap"), "{out}");
        assert!(out.contains("[PROVEN] split on syntax"), "{out}");
        assert!(out.contains("[TRIED] the retry belongs"), "{out}");
    }

    #[test]
    fn a_lesson_cannot_break_the_bullet_list_it_is_rendered_into() {
        let out = render(&[lesson("errors", "line one\nline two")]);
        assert_eq!(out.lines().count(), 1, "one lesson is one bullet: {out:?}");
    }

    #[test]
    fn reading_nothing_asks_for_nothing() {
        // `k = 0` is the control arm and must not cost a round trip.
        let m = Memory {
            url: "http://127.0.0.1:1".into(),
            host: "nowhere".into(),
            timeout: Duration::from_millis(1),
        };
        let cold = Reading { k: 0, budget: 0, pools: vec![], tags: vec![], min_similarity: 0.0 };
        assert!(m.recall("anything", &cold).expect("no call, no failure").is_empty());
        assert!(m.attribute(&[], "run-1", true).is_ok());
    }

    #[test]
    fn a_prior_reads_as_a_sentence_even_with_nothing_to_point_at() {
        let p = Prior {
            goal: "slugify".into(),
            similarity: 0.987_6,
            score: 1000,
            run: "42/g0/risk-first".into(),
            artifact: String::new(),
            evaluations: 3,
        };
        let s = p.summary();
        assert!(s.contains("similarity 0.988"), "{s}");
        assert!(s.contains("nothing addressable"), "a run that left no PR must still say so: {s}");
        assert!(s.contains("3 evaluation(s)"), "{s}");
    }
}
