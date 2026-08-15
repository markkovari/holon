//! The goal runner's client for `knowledge:memory` — skip work already done, and
//! record every verdict so the next run can.
//!
//! ## Nothing here may fail a run
//!
//! Every function answers with a plain value and swallows what went wrong into a
//! `String` the caller may print. That is the whole contract of this module and it
//! is deliberate in one direction: a knowledge pool that is down, misconfigured or
//! simply absent must cost a run nothing but the knowledge. The loop worked before
//! this existed and has to keep working when it is unreachable.
//!
//! The asymmetry that matters: **an error means DO THE WORK.** `already_done`
//! answers `None` when it cannot reach the pool, never "probably done". Redoing
//! work costs money; skipping work that was not done is a silent wrong answer, and
//! the two are not the same kind of mistake (ADR-0084).
//!
//! ## Why HTTP and not a link
//!
//! The reconciler is native and reaches components through the ingress, the same
//! way it reaches the driver and the selector — `memory-probe` in front of
//! `knowledge-memory` is the same shape as `driver-probe` in front of
//! `agent-driver`.

use std::time::Duration;

use serde_json::Value;

/// Where the memory app is, and the ingress host that routes to it.
///
/// `None` is a supported configuration and the default: a run without
/// `--surreal-url` deploys no memory app, makes no calls, and behaves exactly as
/// it did before this module existed.
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
        format!(
            "\"{}\" (similarity {:.3}) already passed at score {} in run {} → {artifact}; \
             {} evaluation(s) on record",
            self.goal, self.similarity, self.score, self.run, self.evaluations
        )
    }
}

fn client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(timeout).build().expect("http client")
}

/// Percent-encode a goal for a query string. Goals are sentences.
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
        let r = client(self.timeout)
            .request(method, format!("{}{path}", self.url))
            .header("host", &self.host)
            .send()
            .map_err(|e| format!("{e}"))?;
        let text = r.text().unwrap_or_default();
        serde_json::from_str(&text).map_err(|e| format!("unreadable answer ({e}): {text}"))
    }

    /// Has this goal already been done? `Ok(None)` means go and do the work.
    ///
    /// Called ONCE per goal, before anything is spawned. Calling it per branch
    /// would be paying for the answer after having already paid for the question.
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

    /// Record one verdict.
    ///
    /// Called once per BRANCH, not once per generation: the count of failed
    /// attempts on a goal is what says whether another generation is worth buying,
    /// and a generation-level record cannot say it. Idempotent per `(goal, run)`,
    /// so the landing path may call it again with the pull request once the forge
    /// has opened one.
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

/// One lesson, as it came back from the pool.
pub struct Lesson {
    /// The handle to attribute an outcome to later. Opaque.
    pub key: String,
    /// `patterns` | `solutions` | `errors`.
    pub ns: String,
    pub text: String,
    pub dense: bool,
}

/// What a branch is allowed to read.
///
/// The DIVERSITY BUDGET, and the reason it is per branch: a generation whose
/// branches all read the same top-k is an expensive way to run one branch
/// (ADR-0081). `k = 0` reads nothing, which is the control arm.
pub struct Reading {
    pub k: u32,
    pub budget: u32,
    /// Empty means all three pools.
    pub pools: Vec<String>,
}

impl Memory {
    /// What the swarm learned that bears on this goal.
    ///
    /// An error is NOT fatal and NOT an empty answer dressed up as one: the caller
    /// gets the error, prints it, and runs the branch cold. A pool that is down
    /// must cost a run its advice and nothing else.
    pub fn recall(&self, goal: &str, r: &Reading) -> Result<Vec<Lesson>, String> {
        if r.k == 0 {
            return Ok(Vec::new());
        }
        let v = self.call(
            reqwest::Method::GET,
            &format!(
                "/recall?goal={}&k={}&budget={}&pools={}",
                enc(goal),
                r.k,
                r.budget,
                enc(&r.pools.join(","))
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

    /// What happened to the lessons a branch read. The ONLY thing that moves an
    /// entry's standing, so a branch that read nothing attributes nothing.
    pub fn attribute(&self, keys: &[String], run: &str, succeeded: bool) -> Result<(), String> {
        if keys.is_empty() {
            return Ok(());
        }
        let v = self.call(
            reqwest::Method::POST,
            &format!(
                "/attribute?keys={}&run={}&ok={succeeded}",
                enc(&keys.join(",")),
                enc(run)
            ),
        )?;
        if let Some(detail) = v["error"].as_str() {
            return Err(format!("{detail}: {}", v["detail"].as_str().unwrap_or_default()));
        }
        Ok(())
    }
}

/// Lessons as a branch sees them: one bullet each, negative knowledge marked.
///
/// The namespace is shown because the three are not worth the same. `errors` is
/// what did not work and is visible to a sibling immediately; `patterns` survived
/// a gate. A model handed both without the distinction weighs them equally.
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
/// It has to be stable for one branch of one search and distinct across them, and
/// it has to mean something to a human reading the graph later — `gen 1 branch
/// risk-first of this search` rather than an opaque number. The seed is in it
/// because that is what makes a branch replayable (ADR-0078).
pub fn run_id(search_seed: u64, round: usize, branch: &str) -> String {
    format!("{search_seed}/g{round}/{branch}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_lesson_says_which_kind_it_is() {
        let out = render(&[
            lesson("errors", "a bare unwrap fails the gate"),
            lesson("patterns", "split on syntax, not token count"),
            lesson("solutions", "the retry belongs in the client"),
        ]);
        // The three are not worth the same, and a model handed them undifferentiated
        // weighs them equally: `errors` is what did not work, `patterns` survived a
        // gate, `solutions` is one branch's word for it.
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
        let cold = Reading { k: 0, budget: 0, pools: vec![] };
        assert!(m.recall("anything", &cold).expect("no call, no failure").is_empty());
        // And a branch that read nothing attributes nothing, for the same reason.
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
