//! `agent-writer` — a goal and a tree in, a candidate out.
//!
//! ## The two halves, and only one of them is the model's
//!
//! Everything a model returns is text. The half that matters here is what
//! happens to that text: which paths are accepted, what an unparseable answer
//! means, and whether a repair actually used the failure it was handed. All of
//! that is pure and tested; the model is whatever the deployment linked.
//!
//! ## The repair loop
//!
//! `previous` carries the checks that failed last time, and their output. That is
//! the only feedback in this system that is not a model's opinion of its own
//! work — `graph:fitness` got it by running real commands — so it goes into the
//! prompt verbatim and first, before the goal is restated.
//!
//! A repair that ignores it is just a re-roll, and the test asserts the
//! difference: the same goal with the same seed produces a DIFFERENT prompt once
//! a failure is known, because otherwise the loop is a retry wearing a costume.
//!
//! ## What is refused
//!
//! A path outside `writable`. The answer comes from a model, and an agent that
//! can name any path is an agent that can rewrite the deployment that runs it.
//! Refused as an error rather than filtered out silently: an answer that touched
//! something it may not is not a partially good answer, it is one nobody should
//! act on.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::agent::writer::{AgentError, Candidate, File, Goal, Guest, Failure};
use bindings::llm::inference::inference as llm;

struct Component;

/// The fences a file block is delimited by.
///
/// A model asked for files returns markdown. Rather than fight that, this asks
/// for a shape markdown already has and parses it — a "return only JSON"
/// instruction is a request a model honours most of the time, which is the worst
/// possible reliability for a parser.
const OPEN: &str = "=== FILE:";
const CLOSE: &str = "=== END";

fn system_prompt() -> String {
    format!(
        "You change code to satisfy a goal.\n\
         \n\
         Answer ONLY with file blocks, in exactly this form:\n\
         {OPEN} path/to/file\n\
         <the complete new contents of that file>\n\
         {CLOSE}\n\
         \n\
         Rules:\n\
         - give the WHOLE file, not a diff and not an excerpt\n\
         - only write files you were told you may write\n\
         - if a file needs no change, leave it out entirely\n\
         - no prose before, between or after the blocks"
    )
}

/// Build what the model is asked.
///
/// Pure, so the interesting question — does a repair differ from a first attempt
/// — is answerable without a model.
fn build_prompt(g: &Goal, previous: &[Failure]) -> String {
    let mut p = String::new();

    // FAILURES FIRST. A repair whose prompt buries what went wrong under the
    // original goal is a repair that will mostly rewrite the original answer.
    if !previous.is_empty() {
        p.push_str("A previous attempt was checked and these checks FAILED.\n");
        p.push_str("Fix these specifically; do not start over.\n\n");
        for f in previous {
            p.push_str(&format!("- {} : {}\n", f.id, f.detail.trim()));
        }
        p.push('\n');
    }

    p.push_str("GOAL\n");
    p.push_str(g.text.trim());
    p.push_str("\n\n");

    if !g.writable.is_empty() {
        p.push_str("You may write ONLY these paths:\n");
        for w in &g.writable {
            p.push_str(&format!("- {w}\n"));
        }
        p.push('\n');
    }

    p.push_str("CURRENT FILES\n");
    for f in &g.context {
        p.push_str(&format!("{OPEN} {}\n{}\n{CLOSE}\n", f.path, f.content));
    }
    p
}

/// Pull file blocks out of whatever came back.
///
/// Tolerant of prose around the blocks, because a model will add it however
/// firmly it was asked not to, and refusing an otherwise good answer over a
/// preamble would throw away work that cost real money.
fn parse_files(answer: &str) -> Vec<File> {
    let mut out = Vec::new();
    let mut rest = answer;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(nl) = after.find('\n') else { break };
        let path = after[..nl].trim().to_string();
        let body = &after[nl + 1..];
        let Some(end) = body.find(CLOSE) else { break };
        let mut content = body[..end].to_string();
        // A model puts a newline before the closing fence; the file should not
        // gain one every time it is rewritten.
        if content.ends_with('\n') {
            content.pop();
        }
        if !path.is_empty() {
            out.push(File { path, content });
        }
        rest = &body[end + CLOSE.len()..];
    }
    out
}

/// May this path be written?
///
/// Exact match against the allow-list. No prefix rule: `src/lib.rs` permitting
/// `src/lib.rs.bak` is the kind of near-miss that reads as fine and is not.
fn writable(g: &Goal, path: &str) -> bool {
    g.writable.iter().any(|w| w == path)
}

impl Guest for Component {
    fn attempt(g: Goal, previous: Vec<Failure>, seed: u64) -> Result<Candidate, AgentError> {
        if g.text.trim().is_empty() {
            return Err(AgentError::UnderSpecified("the goal says nothing".into()));
        }
        if g.writable.is_empty() {
            // An agent with nothing it may write can only produce an answer that
            // is entirely refused, so this is caught where it can be explained.
            return Err(AgentError::UnderSpecified(
                "the goal names no writable paths, so no candidate could be accepted".into(),
            ));
        }

        let opts = llm::Options {
            model: String::new(),
            // Deliberately low. A candidate is judged by a gate, and creativity
            // that fails to compile is not creativity.
            temperature: 200,
            max_tokens: 0,
            stop: Vec::new(),
            // The knob that makes N branches differ while staying replayable.
            seed,
        };
        let messages = vec![
            llm::Message { role: llm::Role::System, content: system_prompt() },
            llm::Message { role: llm::Role::User, content: build_prompt(&g, &previous) },
        ];

        let completion = llm::chat(&messages, &opts).map_err(|e| {
            AgentError::InferenceFailed(match e {
                llm::InferError::InvalidRequest(m) => format!("invalid request: {m}"),
                llm::InferError::ProviderDenied(m) => format!("denied: {m}"),
                llm::InferError::ProviderUnavailable(m) => format!("unavailable: {m}"),
                llm::InferError::BadResponse(m) => format!("bad response: {m}"),
                llm::InferError::NoContent => "the model returned nothing".into(),
            })
        })?;

        let files = parse_files(&completion.text);
        if files.is_empty() {
            // Distinct from an inference failure: the model answered, and the
            // answer was not a candidate. A caller retries those differently —
            // one is worth another seed, the other is worth waiting.
            return Err(AgentError::UnusableAnswer(format!(
                "no file blocks in {} characters of answer",
                completion.text.len()
            )));
        }

        // Refused, not filtered. An answer that wrote somewhere it may not is not
        // a partially good answer — it is one nobody should act on, and silently
        // dropping the offending file would ship the rest of a plan that assumed
        // it.
        if let Some(bad) = files.iter().find(|f| !writable(&g, &f.path)) {
            return Err(AgentError::UnusableAnswer(format!(
                "wrote {:?}, which is not in the writable list",
                bad.path
            )));
        }

        // The cost travels with the answer. A caller that has to ask a second
        // question to find out what the first one cost will eventually forget
        // to, and a budget nobody reports against is not a budget.
        Ok(Candidate {
            files,
            prompt_tokens: completion.usage.prompt_tokens,
            completion_tokens: completion.usage.completion_tokens,
            model: completion.model,
        })
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(text: &str, writable: &[&str], context: &[(&str, &str)]) -> Goal {
        Goal {
            text: text.into(),
            context: context
                .iter()
                .map(|(p, c)| File { path: p.to_string(), content: c.to_string() })
                .collect(),
            writable: writable.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_file_block_becomes_a_file() {
        let files = parse_files(&format!(
            "{OPEN} src/lib.rs\npub fn answer() -> u32 {{ 42 }}\n{CLOSE}\n"
        ));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].content, "pub fn answer() -> u32 { 42 }");
    }

    /// A model adds prose however firmly it is asked not to. Refusing an
    /// otherwise good answer over a preamble would throw away work that cost
    /// money.
    #[test]
    fn prose_around_the_blocks_is_tolerated() {
        let files = parse_files(&format!(
            "Sure! Here is the fix:\n\n{OPEN} a.txt\nhello\n{CLOSE}\n\nLet me know if…"
        ));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "hello");
    }

    #[test]
    fn several_files_come_back_in_order() {
        let files = parse_files(&format!(
            "{OPEN} a.rs\nA\n{CLOSE}\n{OPEN} b.rs\nB\n{CLOSE}\n"
        ));
        assert_eq!(files.len(), 2);
        assert_eq!((files[0].path.as_str(), files[1].path.as_str()), ("a.rs", "b.rs"));
    }

    /// An answer with no blocks is a different failure from the model being
    /// down, and a caller retries them differently.
    #[test]
    fn an_answer_with_no_blocks_yields_nothing() {
        assert!(parse_files("I would suggest refactoring the module.").is_empty());
        // A block that never closes is not half a file.
        assert!(parse_files(&format!("{OPEN} a.rs\nunterminated")).is_empty());
    }

    /// THE REPAIR LOOP. The same goal and the same seed must produce a different
    /// prompt once something is known to have failed — otherwise the second
    /// attempt is a re-roll wearing a costume.
    #[test]
    fn a_repair_prompt_differs_because_of_the_failure() {
        let g = goal("make it 42", &["src/lib.rs"], &[("src/lib.rs", "fn answer() { 41 }")]);
        let first = build_prompt(&g, &[]);
        let repair = build_prompt(
            &g,
            &[Failure { id: "the-fix".into(), detail: "expected 42, found 41".into() }],
        );
        assert_ne!(first, repair, "a repair that reads identically is not a repair");
        assert!(repair.contains("expected 42, found 41"), "the failure must reach the model");
        assert!(repair.contains("the-fix"), "and so must which check it was");
    }

    /// Failures go FIRST. A repair whose prompt buries what went wrong under the
    /// original goal mostly rewrites the original answer.
    #[test]
    fn the_failure_is_stated_before_the_goal_is_restated() {
        let g = goal("make it 42", &["a.rs"], &[]);
        let p = build_prompt(&g, &[Failure { id: "x".into(), detail: "boom".into() }]);
        assert!(
            p.find("boom").unwrap() < p.find("GOAL").unwrap(),
            "what failed must come before what was wanted"
        );
    }

    #[test]
    fn the_prompt_carries_the_files_and_the_writable_list() {
        let g = goal("x", &["a.rs", "new.rs"], &[("a.rs", "contents here")]);
        let p = build_prompt(&g, &[]);
        assert!(p.contains("contents here"), "the model needs the current file");
        assert!(p.contains("new.rs"), "including a path that does not exist yet");
    }

    /// The answer comes from a model. An agent that may write any path is an
    /// agent that may rewrite the deployment running it.
    #[test]
    fn a_path_outside_the_allow_list_is_refused_not_filtered() {
        let g = goal("x", &["src/lib.rs"], &[]);
        assert!(writable(&g, "src/lib.rs"));
        assert!(!writable(&g, "src/other.rs"));
        // No prefix rule: a near-miss that reads as fine is exactly the kind that
        // is not.
        assert!(!writable(&g, "src/lib.rs.bak"));
        assert!(!writable(&g, "../etc/passwd"));
    }
}
