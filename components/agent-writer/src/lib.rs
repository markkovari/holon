//! `agent-writer` — a goal and a tree in, a candidate out.
//!
//! ## The two halves, and only one of them is the model's
//!
//! Everything a model returns is text. The half that matters here is what
//! happens to that text: which paths are accepted, what an unparseable answer
//! means, and whether a repair actually used the failure it was handed. All of
//! that is pure and tested; the model is whatever the deployment linked.
//!
//! ## Edits, not whole files
//!
//! The model answers with search/replace EDIT blocks — only the lines that
//! change — and the writer applies them against the files it already holds
//! (`goal.context`) to reconstruct the whole file it ships. Whole files stay on
//! the wire (the gate and the forge are unchanged); what shrinks is the model's
//! OUTPUT, the expensive tokens, and with it the failure mode where a whole-file
//! rewrite of a 300-line file silently drops a function nobody asked it to touch.
//!
//! An edit whose SEARCH text is not in the file is a hard error, not a skip: a
//! diff that does not apply is exactly the case whole-file rewriting turned into
//! silent corruption. Here it fails the candidate, and the driver repairs it.
//! A FILE block still exists for creating a new file or replacing most of one.
//!
//! ## The repair loop
//!
//! `previous` carries the checks that failed last time, and their output. That is
//! the only feedback in this system that is not a model's opinion of its own
//! work — `graph:fitness` got it by running real commands — so it goes into the
//! prompt verbatim and first, before the goal is restated.
//!
//! ## What is refused
//!
//! A path outside `writable`. The answer comes from a model, and an agent that
//! can name any path is an agent that can rewrite the deployment that runs it.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::agent::writer::{AgentError, Candidate, File, Goal, Guest, Failure};
use bindings::llm::inference::inference as llm;
use std::collections::HashMap;

struct Component;

// A whole-file block, for creating a file or replacing most of one.
const FILE_OPEN: &str = "=== FILE:";
const FILE_CLOSE: &str = "=== END";
// An edit block: a path, then a git-conflict-shaped search/replace the model
// already knows how to produce.
const EDIT_OPEN: &str = "=== EDIT:";
const S_MARK: &str = "<<<<<<< SEARCH";
const DIV: &str = "=======";
const R_MARK: &str = ">>>>>>> REPLACE";

/// What the model is told about the answer FORMAT.
///
/// A worked example, not a schematic — and that is a measured choice, not a
/// stylistic one. The schematic this replaced (`the exact existing lines to
/// replace`) was followed by Claude and NOT by Qwen3-Coder-30B, which answered a
/// real goal with `=== EDIT: path` followed by a ```rust fence and no conflict
/// markers at all. The parser found no `SEARCH` and discarded the whole answer,
/// and the branch was recorded as "no edit or file blocks" — a format failure
/// wearing the costume of a model that cannot code.
///
/// Three things fixed it, verified against that model on the same prompt: an
/// example with real code in it, naming the four marker lines as mandatory, and
/// saying explicitly that markdown fences are not allowed. A model that has spent
/// its life emitting ```rust needs to be told this file is not a chat window.
fn system_prompt() -> String {
    format!(
        "You change code to satisfy a goal. You answer ONLY with blocks.\n\
         \n\
         An edit block looks EXACTLY like this, with all four marker lines\n\
         present:\n\
         \n\
         {EDIT_OPEN} src/example.rs\n\
         {S_MARK}\n\
         fn greet() -> &'static str {{\n\
             \"hi\"\n\
         }}\n\
         {DIV}\n\
         fn greet() -> &'static str {{\n\
             \"hello\"\n\
         }}\n\
         {R_MARK}\n\
         \n\
         The four marker lines are mandatory and must appear in this order:\n\
           \"{EDIT_OPEN} <path>\", then \"{S_MARK}\", then \"{DIV}\", then\n\
           \"{R_MARK}\". An answer missing any of them is discarded unread.\n\
         \n\
         - NEVER use markdown code fences. No ```rust, no ```. The code goes\n\
           bare between the marker lines.\n\
         - the SEARCH text must be copied character-for-character from the file\n\
           shown to you, and be several lines long so it occurs exactly once\n\
         - use one edit block per change; emit as many as you need\n\
         - PREFER edit blocks: emit only the lines that change, never a whole\n\
           file\n\
         \n\
         To CREATE a new file, or when the change is most of a file, give the\n\
         whole contents instead:\n\
         {FILE_OPEN} path/to/file\n\
         <the complete new contents>\n\
         {FILE_CLOSE}\n\
         \n\
         Rules:\n\
         - only write files you were told you may write\n\
         - if a file needs no change, leave it out entirely\n\
         - write no prose. Your entire answer is blocks."
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
        p.push_str(&format!("{FILE_OPEN} {}\n{}\n{FILE_CLOSE}\n", f.path, f.content));
    }
    p
}

/// One change the model asked for: either a whole file, or a search/replace.
enum Op {
    Whole { path: String, content: String },
    Edit { path: String, search: String, replace: String },
}

fn op_path(op: &Op) -> &str {
    match op {
        Op::Whole { path, .. } | Op::Edit { path, .. } => path,
    }
}

/// Pull ops out of whatever came back, in document order.
///
/// Tolerant of prose around the blocks, because a model will add it however
/// firmly it was asked not to, and refusing an otherwise good answer over a
/// preamble would throw away work that cost real money. Order is preserved so
/// that several edits to the same file apply as the model intended.
fn parse_ops(answer: &str) -> Vec<Op> {
    let mut out = Vec::new();
    let mut rest = answer;
    loop {
        let nf = rest.find(FILE_OPEN);
        let ne = rest.find(EDIT_OPEN);
        let use_edit = match (nf, ne) {
            (None, None) => break,
            (Some(_), None) => false,
            (None, Some(_)) => true,
            (Some(f), Some(e)) => e < f,
        };

        if use_edit {
            let start = ne.unwrap();
            let after = &rest[start + EDIT_OPEN.len()..];
            let Some(nl) = after.find('\n') else { break };
            let path = after[..nl].trim().to_string();
            let b = &after[nl + 1..];
            let Some(sm) = b.find(S_MARK) else { break };
            let asm = &b[sm + S_MARK.len()..];
            let Some(snl) = asm.find('\n') else { break };
            let sbody = &asm[snl + 1..];
            let Some(dv) = sbody.find(DIV) else { break };
            let mut search = sbody[..dv].to_string();
            // The newline before the divider belongs to the fence, not the text.
            if search.ends_with('\n') {
                search.pop();
            }
            let adv = &sbody[dv + DIV.len()..];
            let Some(dnl) = adv.find('\n') else { break };
            let rbody = &adv[dnl + 1..];
            let Some(rm) = rbody.find(R_MARK) else { break };
            let mut replace = rbody[..rm].to_string();
            if replace.ends_with('\n') {
                replace.pop();
            }
            if !path.is_empty() {
                out.push(Op::Edit { path, search, replace });
            }
            rest = &rbody[rm + R_MARK.len()..];
        } else {
            let start = nf.unwrap();
            let after = &rest[start + FILE_OPEN.len()..];
            let Some(nl) = after.find('\n') else { break };
            let path = after[..nl].trim().to_string();
            let body = &after[nl + 1..];
            let Some(end) = body.find(FILE_CLOSE) else { break };
            let mut content = body[..end].to_string();
            // A model puts a newline before the closing fence; the file should
            // not gain one every time it is rewritten.
            if content.ends_with('\n') {
                content.pop();
            }
            if !path.is_empty() {
                out.push(Op::Whole { path, content });
            }
            rest = &body[end + FILE_CLOSE.len()..];
        }
    }
    out
}

/// Replace the first occurrence of `search` in `cur` with `replace`.
///
/// Exact first, then a whitespace-tolerant retry that matches line-by-line
/// ignoring trailing whitespace — the single most common way a model's copy of
/// the SEARCH text drifts from the file. Returns `None` when the text is nowhere
/// to be found, which the caller turns into a rejected candidate.
fn find_replace(cur: &str, search: &str, replace: &str) -> Option<String> {
    if let Some(pos) = cur.find(search) {
        let mut s = String::with_capacity(cur.len() - search.len() + replace.len());
        s.push_str(&cur[..pos]);
        s.push_str(replace);
        s.push_str(&cur[pos + search.len()..]);
        return Some(s);
    }

    let cur_lines: Vec<&str> = cur.lines().collect();
    let s_lines: Vec<&str> = search.lines().collect();
    if s_lines.is_empty() || s_lines.len() > cur_lines.len() {
        return None;
    }
    let matches = |start: usize| {
        (0..s_lines.len()).all(|k| cur_lines[start + k].trim_end() == s_lines[k].trim_end())
    };
    let start = (0..=cur_lines.len() - s_lines.len()).find(|&i| matches(i))?;

    let mut out: Vec<&str> = Vec::new();
    out.extend_from_slice(&cur_lines[..start]);
    out.extend(replace.lines());
    out.extend_from_slice(&cur_lines[start + s_lines.len()..]);
    let mut s = out.join("\n");
    // Preserve the file's trailing newline; `lines()` drops it.
    if cur.ends_with('\n') {
        s.push('\n');
    }
    Some(s)
}

/// Fold the ops onto the base tree, producing the whole contents of every file
/// that was touched, in first-touch order.
fn apply_ops(base: &[File], ops: Vec<Op>) -> Result<Vec<File>, String> {
    let mut work: HashMap<String, String> =
        base.iter().map(|f| (f.path.clone(), f.content.clone())).collect();
    let mut order: Vec<String> = Vec::new();
    let touch = |order: &mut Vec<String>, path: &str| {
        if !order.iter().any(|p| p == path) {
            order.push(path.to_string());
        }
    };

    for op in ops {
        match op {
            Op::Whole { path, content } => {
                touch(&mut order, &path);
                work.insert(path, content);
            }
            Op::Edit { path, search, replace } => {
                touch(&mut order, &path);
                if search.is_empty() {
                    // An empty SEARCH is "create or overwrite" — same as a FILE
                    // block, tolerated so a model that reaches for the edit shape
                    // for a new file is not punished for it.
                    work.insert(path, replace);
                    continue;
                }
                let cur = work.get(&path).cloned().unwrap_or_default();
                let next = find_replace(&cur, &search, &replace).ok_or_else(|| {
                    format!("edit to {path:?}: its SEARCH block is not in the file")
                })?;
                work.insert(path, next);
            }
        }
    }

    Ok(order
        .into_iter()
        .map(|path| {
            let content = work.remove(&path).unwrap_or_default();
            File { path, content }
        })
        .collect())
}

/// The temperature to request at a given repair depth, in milli-units.
///
/// Low first (0.2): a first attempt should be the model's most likely answer,
/// which a determinable gate rewards. Each repair raises it — a branch that
/// failed and is re-trying wants to EXPLORE, not re-roll the same near-miss — up
/// to a 1.0 ceiling. The provider only forwards this to models that accept a
/// temperature; on the rest it is a no-op, so escalating is always safe to ask.
fn temperature_for(repair_depth: u32) -> u32 {
    (200 + 300 * repair_depth).min(1000)
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
            // Low on the first try, higher on each repair — a stuck branch should
            // explore, not re-roll. The provider withholds it from models that
            // rejected it, so asking is always safe.
            temperature: temperature_for(previous.len() as u32),
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

        let ops = parse_ops(&completion.text);
        if ops.is_empty() {
            // Distinct from an inference failure: the model answered, and the
            // answer was not a candidate. A caller retries those differently —
            // one is worth another seed, the other is worth waiting.
            return Err(AgentError::UnusableAnswer(format!(
                "no edit or file blocks in {} characters of answer; starts: {:?}",
                completion.text.len(),
                completion.text.chars().take(160).collect::<String>()
            )));
        }

        // Refused, not filtered — checked on the ops' target paths, BEFORE any
        // edit is applied, so an answer that names a path it may not touch never
        // runs against the tree.
        if let Some(bad) = ops.iter().map(op_path).find(|p| !writable(&g, p)) {
            return Err(AgentError::UnusableAnswer(format!(
                "wrote {bad:?}, which is not in the writable list"
            )));
        }

        // A diff that does not apply is the whole reason edits are safer than
        // whole-file rewrites: it fails loudly here instead of shipping a file
        // with a function silently missing.
        let files = apply_ops(&g.context, ops).map_err(AgentError::UnusableAnswer)?;

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

    /// Parse + apply, the path a candidate actually travels.
    fn run(answer: &str, base: &[(&str, &str)]) -> Result<Vec<File>, String> {
        let base: Vec<File> =
            base.iter().map(|(p, c)| File { path: p.to_string(), content: c.to_string() }).collect();
        apply_ops(&base, parse_ops(answer))
    }

    #[test]
    fn a_file_block_becomes_a_file() {
        let files = run(
            &format!("{FILE_OPEN} src/lib.rs\npub fn answer() -> u32 {{ 42 }}\n{FILE_CLOSE}\n"),
            &[],
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].content, "pub fn answer() -> u32 { 42 }");
    }

    /// The point of #2: an edit touches only the lines it names, and the rest of
    /// the file survives verbatim.
    #[test]
    fn an_edit_changes_only_its_lines() {
        let base = [("src/lib.rs", "fn a() {}\nfn answer() -> u32 { 41 }\nfn b() {}\n")];
        let files = run(
            &format!(
                "{EDIT_OPEN} src/lib.rs\n{S_MARK}\nfn answer() -> u32 {{ 41 }}\n{DIV}\nfn answer() -> u32 {{ 42 }}\n{R_MARK}\n"
            ),
            &base,
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "fn a() {}\nfn answer() -> u32 { 42 }\nfn b() {}\n");
    }

    /// A SEARCH that is not in the file is a hard error — the candidate is
    /// rejected, not shipped with a botched edit.
    #[test]
    fn an_edit_that_does_not_match_is_an_error() {
        let base = [("a.rs", "hello\n")];
        let err = run(
            &format!("{EDIT_OPEN} a.rs\n{S_MARK}\ngoodbye\n{DIV}\nhi\n{R_MARK}\n"),
            &base,
        )
        .unwrap_err();
        assert!(err.contains("not in the file"), "a diff that will not apply must fail loudly: {err}");
    }

    /// The model rarely reproduces trailing whitespace exactly; a match that
    /// differs only there should still apply.
    #[test]
    fn trailing_whitespace_does_not_break_a_match() {
        // The file is clean; the model's SEARCH carries trailing spaces, so the
        // exact substring search misses and the line-normalized retry catches it.
        let base = [("a.rs", "let x = 1;\nlet y = 2;\n")];
        let files = run(
            &format!("{EDIT_OPEN} a.rs\n{S_MARK}\nlet x = 1;   \n{DIV}\nlet x = 9;\n{R_MARK}\n"),
            &base,
        )
        .unwrap();
        assert_eq!(files[0].content, "let x = 9;\nlet y = 2;\n");
    }

    /// Several edits to one file apply in order and accumulate.
    #[test]
    fn several_edits_to_one_file_accumulate() {
        let base = [("a.rs", "one\ntwo\nthree\n")];
        let files = run(
            &format!(
                "{EDIT_OPEN} a.rs\n{S_MARK}\none\n{DIV}\n1\n{R_MARK}\n\
                 {EDIT_OPEN} a.rs\n{S_MARK}\nthree\n{DIV}\n3\n{R_MARK}\n"
            ),
            &base,
        )
        .unwrap();
        assert_eq!(files.len(), 1, "one file, touched twice");
        assert_eq!(files[0].content, "1\ntwo\n3\n");
    }

    #[test]
    fn prose_around_the_blocks_is_tolerated() {
        let files = run(
            &format!("Sure! Here is the fix:\n\n{FILE_OPEN} a.txt\nhello\n{FILE_CLOSE}\n\nLet me know if…"),
            &[],
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "hello");
    }

    #[test]
    fn several_files_come_back_in_order() {
        let files = run(
            &format!("{FILE_OPEN} a.rs\nA\n{FILE_CLOSE}\n{FILE_OPEN} b.rs\nB\n{FILE_CLOSE}\n"),
            &[],
        )
        .unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!((files[0].path.as_str(), files[1].path.as_str()), ("a.rs", "b.rs"));
    }

    #[test]
    fn an_answer_with_no_blocks_yields_nothing() {
        assert!(parse_ops("I would suggest refactoring the module.").is_empty());
        // A block that never closes is not half a file.
        assert!(parse_ops(&format!("{FILE_OPEN} a.rs\nunterminated")).is_empty());
        // An edit missing its REPLACE fence is not half an edit.
        assert!(parse_ops(&format!("{EDIT_OPEN} a.rs\n{S_MARK}\nx\n{DIV}\ny\n")).is_empty());
    }

    /// THE REPAIR LOOP. The same goal and the same seed must produce a different
    /// prompt once something is known to have failed.
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

    /// Low first, rising with each repair, capped at 1.0 — a stuck branch
    /// explores instead of re-rolling the same near-miss.
    #[test]
    fn temperature_escalates_with_repair_depth_and_caps() {
        assert_eq!(temperature_for(0), 200, "first attempt is the likely answer");
        assert_eq!(temperature_for(1), 500);
        assert_eq!(temperature_for(2), 800);
        assert_eq!(temperature_for(3), 1000, "capped at 1.0");
        assert_eq!(temperature_for(99), 1000);
    }

    #[test]
    fn a_path_outside_the_allow_list_is_refused_not_filtered() {
        let g = goal("x", &["src/lib.rs"], &[]);
        assert!(writable(&g, "src/lib.rs"));
        assert!(!writable(&g, "src/other.rs"));
        assert!(!writable(&g, "src/lib.rs.bak"));
        assert!(!writable(&g, "../etc/passwd"));
    }
}
