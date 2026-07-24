//! `textdiff` — reference implementation of `diff:text`.
//!
//! Line-based diff. The edit script comes from a longest-common-subsequence
//! backtrack; `unified` groups the script into hunks with context, and
//! `apply-unified` replays a unified diff against a source, checking every
//! context/delete line. Round-trip holds: `apply-unified(a, unified(a,b)) == b`.
//!
//! Pure compute, no host imports, no state.

#[allow(warnings)]
mod bindings;

use bindings::exports::diff::text::differ::{DiffError, Guest, Op};

struct Component;

/// Split into lines on `\n`. Empty input is zero lines (not one empty line);
/// a trailing `\n` becomes a trailing empty line, so `join("\n")` restores the
/// exact bytes.
fn lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split('\n').collect()
    }
}

/// LCS edit script over lines.
// ponytail: O(n*m) LCS table — fine for docs/snippets; swap for Myers O(ND) if
// this ever diffs large files.
fn edit_script<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    // dp[i][j] = LCS length of a[i..], b[j..]
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Equal(a[i].to_string()));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Delete(a[i].to_string()));
            i += 1;
        } else {
            ops.push(Op::Insert(b[j].to_string()));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(a[i].to_string()));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(b[j].to_string()));
        j += 1;
    }
    ops
}

/// A positioned row: tag + 0-based source (a) and target (b) line indices.
struct Row {
    op: Op,
    ai: usize,
    bi: usize,
}

fn rows_of(ops: Vec<Op>) -> Vec<Row> {
    let (mut ai, mut bi) = (0, 0);
    ops.into_iter()
        .map(|op| {
            let row = Row { ai, bi, op: op.clone() };
            match &op {
                Op::Equal(_) => {
                    ai += 1;
                    bi += 1;
                }
                Op::Delete(_) => ai += 1,
                Op::Insert(_) => bi += 1,
            }
            row
        })
        .collect()
}

fn is_change(op: &Op) -> bool {
    !matches!(op, Op::Equal(_))
}

fn build_unified(a: &str, b: &str, from: &str, to: &str, context: usize) -> String {
    let (al, bl) = (lines(a), lines(b));
    let rows = rows_of(edit_script(&al, &bl));
    if !rows.iter().any(|r| is_change(&r.op)) {
        return String::new(); // identical
    }
    // Keep every row within `context` of a change; contiguous kept runs = hunks.
    let n = rows.len();
    let mut keep = vec![false; n];
    for (idx, r) in rows.iter().enumerate() {
        if is_change(&r.op) {
            let lo = idx.saturating_sub(context);
            let hi = (idx + context).min(n - 1);
            keep[lo..=hi].iter_mut().for_each(|k| *k = true);
        }
    }

    let mut out = format!("--- {from}\n+++ {to}\n");
    let mut s = 0;
    while s < n {
        if !keep[s] {
            s += 1;
            continue;
        }
        let mut e = s;
        while e + 1 < n && keep[e + 1] {
            e += 1;
        }
        let run = &rows[s..=e];
        let a_count = run.iter().filter(|r| !matches!(r.op, Op::Insert(_))).count();
        let b_count = run.iter().filter(|r| !matches!(r.op, Op::Delete(_))).count();
        // 1-based start; for a pure-insert run use the insertion point.
        let a_start = run
            .iter()
            .find(|r| !matches!(r.op, Op::Insert(_)))
            .map(|r| r.ai + 1)
            .unwrap_or(run[0].ai + 1);
        let b_start = run
            .iter()
            .find(|r| !matches!(r.op, Op::Delete(_)))
            .map(|r| r.bi + 1)
            .unwrap_or(run[0].bi + 1);
        out.push_str(&format!(
            "@@ -{a_start},{a_count} +{b_start},{b_count} @@\n"
        ));
        for r in run {
            match &r.op {
                Op::Equal(t) => out.push_str(&format!(" {t}\n")),
                Op::Delete(t) => out.push_str(&format!("-{t}\n")),
                Op::Insert(t) => out.push_str(&format!("+{t}\n")),
            }
        }
        s = e + 1;
    }
    out
}

/// Parse the source start line (1-based) from a hunk header body (text after
/// the leading `@@`): `... -A,S +B,S @@`.
fn hunk_a_start(h: &str) -> Option<usize> {
    let minus = h.find('-')?;
    let rest = &h[minus + 1..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn apply(a: &str, patch: &str) -> Result<String, DiffError> {
    let al = lines(a);
    let mut out: Vec<String> = Vec::new();
    let mut ai = 0usize;
    let mut it = patch.lines().peekable();
    while let Some(line) = it.next() {
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        let Some(header) = line.strip_prefix("@@") else {
            continue; // ignore anything outside a hunk
        };
        let start = hunk_a_start(header)
            .ok_or_else(|| DiffError::MalformedPatch(format!("bad hunk header: {line}")))?;
        let target = start.saturating_sub(1); // 0-based first source line of hunk
        if target > al.len() {
            return Err(DiffError::ContextMismatch(format!(
                "hunk starts at line {start}, past end of source ({})",
                al.len()
            )));
        }
        while ai < target {
            out.push(al[ai].to_string());
            ai += 1;
        }
        // hunk body: until the next header or EOF
        while let Some(&body) = it.peek() {
            if body.starts_with("@@") {
                break;
            }
            it.next();
            // A context line is " text"; a stray empty line is treated as
            // context of "".
            if body.is_empty() || body.starts_with(' ') {
                let t = body.get(1..).unwrap_or("");
                if ai >= al.len() || al[ai] != t {
                    return Err(DiffError::ContextMismatch(format!(
                        "context line does not match source at line {}",
                        ai + 1
                    )));
                }
                out.push(al[ai].to_string());
                ai += 1;
            } else if let Some(t) = body.strip_prefix('-') {
                if ai >= al.len() || al[ai] != t {
                    return Err(DiffError::ContextMismatch(format!(
                        "deleted line does not match source at line {}",
                        ai + 1
                    )));
                }
                ai += 1;
            } else if let Some(t) = body.strip_prefix('+') {
                out.push(t.to_string());
            } else if body.starts_with('\\') {
                // "\ No newline at end of file" — ignore.
            } else {
                return Err(DiffError::MalformedPatch(format!(
                    "unrecognized patch line: {body}"
                )));
            }
        }
    }
    while ai < al.len() {
        out.push(al[ai].to_string());
        ai += 1;
    }
    Ok(out.join("\n"))
}

impl Guest for Component {
    fn diff_lines(a: String, b: String) -> Vec<Op> {
        edit_script(&lines(&a), &lines(&b))
    }

    fn unified(a: String, b: String, from_label: String, to_label: String, context: u32) -> String {
        build_unified(&a, &b, &from_label, &to_label, context as usize)
    }

    fn apply_unified(a: String, patch: String) -> Result<String, DiffError> {
        apply(&a, &patch)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn u(a: &str, b: &str, ctx: usize) -> String {
        build_unified(a, b, "a", "b", ctx)
    }

    #[test]
    fn identical_texts_yield_empty_diff() {
        assert_eq!(u("x\ny", "x\ny", 3), "");
        assert!(matches!(edit_script(&lines("x"), &lines("x"))[0], Op::Equal(_)));
    }

    #[test]
    fn edit_script_classifies_lines() {
        let ops = edit_script(&lines("a\nb\nc"), &lines("a\nB\nc"));
        // a=equal, b->B is delete+insert, c=equal
        let tags: Vec<&str> = ops
            .iter()
            .map(|o| match o {
                Op::Equal(_) => "=",
                Op::Insert(_) => "+",
                Op::Delete(_) => "-",
            })
            .collect();
        assert_eq!(tags, ["=", "-", "+", "="]);
    }

    /// The property that matters: a unified diff applied to the source
    /// reproduces the target, across many context sizes and edit shapes.
    #[test]
    fn roundtrip_apply_reproduces_target() {
        let cases = [
            ("", "hello"),                                   // empty -> content
            ("hello", ""),                                   // content -> empty
            ("a\nb\nc\nd\ne", "a\nB\nc\nd\ne"),              // single change
            ("a\nb\nc\nd\ne", "a\nc\nd\ne\nf"),              // delete + append
            ("one\ntwo\nthree", "zero\none\ntwo\nthree"),    // prepend
            ("keep\ndrop\nkeep", "keep\nkeep"),              // middle delete
            ("x\ny\nz\n", "x\nY\nz\n"),                      // trailing newline preserved
            ("line", "totally\ndifferent\ntext"),           // full rewrite
        ];
        for (a, b) in cases {
            for ctx in [0usize, 1, 3] {
                let patch = u(a, b, ctx);
                let got = apply(a, &patch).expect("patch applies");
                assert_eq!(got, b, "roundtrip failed: a={a:?} b={b:?} ctx={ctx}\n{patch}");
            }
        }
    }

    #[test]
    fn apply_rejects_a_patch_that_does_not_fit() {
        let patch = u("a\nb\nc", "a\nB\nc", 1);
        let err = apply("a\nDIFFERENT\nc", &patch).unwrap_err();
        assert!(matches!(err, DiffError::ContextMismatch(_)));
    }
}
