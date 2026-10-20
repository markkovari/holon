//! `agent-writer` — a goal and a tree in, a candidate out.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::agent::writer::{
    AgentError, Blocked, Candidate, Failure, File, Goal, Guest,
};
use bindings::llm::inference::inference as llm;
use std::collections::HashMap;

struct Component;

const FILE_OPEN: &str = "=== FILE:";
const FILE_CLOSE: &str = "=== END";
// Git-conflict-shaped: a format models already produce.
const EDIT_OPEN: &str = "=== EDIT:";
const S_MARK: &str = "<<<<<<< SEARCH";
const DIV: &str = "=======";
const R_MARK: &str = ">>>>>>> REPLACE";

/// A worked example, not a schematic: Qwen3-Coder ignored the schematic and fenced its answer.
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
         - write no prose. Your entire answer is blocks.\n\
         - YOU CANNOT READ FILES. There are no tools here and there is no second\n\
           turn. Everything you are getting is already below under CURRENT\n\
           FILES; if something you want is not there, work from what is. An\n\
           answer that asks to open a file is discarded unread, and asking costs\n\
           the attempt."
    )
}

/// Pure, so whether a repair differs from a first attempt is answerable without a model.
fn build_prompt(g: &Goal, previous: &[Failure], blocked: &[Blocked]) -> String {
    let mut p = String::new();

    // Failures first: a repair that reads the goal first mostly rewrites the original answer.
    if !previous.is_empty() {
        p.push_str("A previous attempt was checked and these checks FAILED.\n");
        p.push_str("Fix these specifically; do not start over.\n\n");
        for f in previous {
            p.push_str(&format!("- {} : {}\n", f.id, f.detail.trim()));
        }
        p.push('\n');
    }

    // After the failures, before the goal, as "not attempted": else a model "repairs" a test that never ran.
    if !blocked.is_empty() {
        p.push_str("These checks were NOT ATTEMPTED, because something they need failed first.\n");
        p.push_str("Do not try to fix them — fix what failed, and these will run.\n\n");
        for b in blocked {
            p.push_str(&format!("- {} (waiting on {})\n", b.id, b.needs));
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

enum Op {
    Whole { path: String, content: String },
    Edit { path: String, search: String, replace: String },
}

fn op_path(op: &Op) -> &str {
    match op {
        Op::Whole { path, .. } | Op::Edit { path, .. } => path,
    }
}

/// Tolerant of prose around blocks: refusing a good answer over a preamble wastes paid work.
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
            // Models put a newline before the closing fence; the file must not gain one per rewrite.
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

/// Exact first, then line-by-line ignoring trailing whitespace, the commonest drift in a model's SEARCH.
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
                    // An empty SEARCH means create or overwrite, same as a FILE block.
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

/// Low first so a first attempt is the likeliest answer, rising to 1.0 so a failed branch explores.
fn temperature_for(repair_depth: u32) -> u32 {
    (200 + 300 * repair_depth).min(1000)
}

/// Exact match, no prefix rule: `src/lib.rs` must not permit `src/lib.rs.bak`.
fn writable(g: &Goal, path: &str) -> bool {
    g.writable.iter().any(|w| w == path)
}

impl Guest for Component {
    fn attempt(
        g: Goal,
        previous: Vec<Failure>,
        blocked: Vec<Blocked>,
        seed: u64,
    ) -> Result<Candidate, AgentError> {
        if g.text.trim().is_empty() {
            return Err(AgentError::UnderSpecified("the goal says nothing".into()));
        }
        if g.writable.is_empty() {
            // Nothing writable means every answer is refused; caught where it can be explained.
            return Err(AgentError::UnderSpecified(
                "the goal names no writable paths, so no candidate could be accepted".into(),
            ));
        }

        let opts = llm::Options {
            model: String::new(),
            temperature: temperature_for(previous.len() as u32),
            max_tokens: 0,
            stop: Vec::new(),
            // The knob that makes N branches differ while staying replayable.
            seed,
        };
        let messages = vec![
            llm::Message { role: llm::Role::System, content: system_prompt() },
            llm::Message { role: llm::Role::User, content: build_prompt(&g, &previous, &blocked) },
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
            // Distinct from an inference failure: one is worth another seed, the other is worth waiting.
            return Err(AgentError::UnusableAnswer(format!(
                "no edit or file blocks in {} characters of answer; starts: {:?}",
                completion.text.len(),
                completion.text.chars().take(160).collect::<String>()
            )));
        }

        // Refused before any edit is applied, so a forbidden path never runs against the tree.
        if let Some(bad) = ops.iter().map(op_path).find(|p| !writable(&g, p)) {
            return Err(AgentError::UnusableAnswer(format!(
                "wrote {bad:?}, which is not in the writable list"
            )));
        }

        // A diff that does not apply fails here instead of shipping a file with a function missing.
        let files = apply_ops(&g.context, ops).map_err(AgentError::UnusableAnswer)?;

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

    fn run(answer: &str, base: &[(&str, &str)]) -> Result<Vec<File>, String> {
        let base: Vec<File> = base
            .iter()
            .map(|(p, c)| File { path: p.to_string(), content: c.to_string() })
            .collect();
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

    #[test]
    fn an_edit_that_does_not_match_is_an_error() {
        let base = [("a.rs", "hello\n")];
        let err =
            run(&format!("{EDIT_OPEN} a.rs\n{S_MARK}\ngoodbye\n{DIV}\nhi\n{R_MARK}\n"), &base)
                .unwrap_err();
        assert!(
            err.contains("not in the file"),
            "a diff that will not apply must fail loudly: {err}"
        );
    }

    #[test]
    fn trailing_whitespace_does_not_break_a_match() {
        let base = [("a.rs", "let x = 1;\nlet y = 2;\n")];
        let files = run(
            &format!("{EDIT_OPEN} a.rs\n{S_MARK}\nlet x = 1;   \n{DIV}\nlet x = 9;\n{R_MARK}\n"),
            &base,
        )
        .unwrap();
        assert_eq!(files[0].content, "let x = 9;\nlet y = 2;\n");
    }

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
        assert!(parse_ops(&format!("{FILE_OPEN} a.rs\nunterminated")).is_empty());
        assert!(parse_ops(&format!("{EDIT_OPEN} a.rs\n{S_MARK}\nx\n{DIV}\ny\n")).is_empty());
    }

    #[test]
    fn a_repair_prompt_differs_because_of_the_failure() {
        let g = goal("make it 42", &["src/lib.rs"], &[("src/lib.rs", "fn answer() { 41 }")]);
        let first = build_prompt(&g, &[], &[]);
        let repair = build_prompt(
            &g,
            &[Failure { id: "the-fix".into(), detail: "expected 42, found 41".into() }],
            &[],
        );
        assert_ne!(first, repair, "a repair that reads identically is not a repair");
        assert!(repair.contains("expected 42, found 41"), "the failure must reach the model");
        assert!(repair.contains("the-fix"), "and so must which check it was");
    }

    #[test]
    fn a_blocked_check_is_named_but_not_asked_for() {
        let g = goal("make it 42", &["a.rs"], &[]);
        let p = build_prompt(
            &g,
            &[Failure { id: "compiles".into(), detail: "E0308".into() }],
            &[Blocked { id: "tests".into(), needs: "compiles".into() }],
        );
        assert!(p.contains("tests"), "the blocked check is named");
        assert!(p.contains("compiles"), "and so is what it waits on");
        assert!(
            p.contains("NOT ATTEMPTED"),
            "and it says so — a blocked check listed among failures is one a model will try to fix"
        );
        assert!(
            p.contains("Do not try to fix them"),
            "explicitly, because the model will otherwise"
        );
        assert!(p.find("E0308").unwrap() < p.find("NOT ATTEMPTED").unwrap());
        assert!(p.find("NOT ATTEMPTED").unwrap() < p.find("GOAL").unwrap());

        let clean = build_prompt(&g, &[], &[]);
        assert!(!clean.contains("NOT ATTEMPTED"));
    }

    #[test]
    fn the_failure_is_stated_before_the_goal_is_restated() {
        let g = goal("make it 42", &["a.rs"], &[]);
        let p = build_prompt(&g, &[Failure { id: "x".into(), detail: "boom".into() }], &[]);
        assert!(
            p.find("boom").unwrap() < p.find("GOAL").unwrap(),
            "what failed must come before what was wanted"
        );
    }

    #[test]
    fn the_prompt_carries_the_files_and_the_writable_list() {
        let g = goal("x", &["a.rs", "new.rs"], &[("a.rs", "contents here")]);
        let p = build_prompt(&g, &[], &[]);
        assert!(p.contains("contents here"), "the model needs the current file");
        assert!(p.contains("new.rs"), "including a path that does not exist yet");
    }

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
