//! The four tools an agent gets, all rooted at its session's workspace.
//!
//! The file tools are CONFINED: a path is relative, has no `..`, and after
//! symlinks are resolved still lands inside the workspace. `run` is not
//! confinable — a shell can `cd /` — and that is what approval is for: a
//! session only skips asking for the tools its creator listed.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// What a tool returns to the model. Output is text either way.
pub struct Outcome {
    pub output: String,
    pub is_error: bool,
}

/// Caps what one tool call can hand back, so a `cat` of a large file does not
/// become the next prompt's entire budget.
const MAX_OUTPUT: usize = 64 * 1024;
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Execution safety tier for commands run through the shell tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTier {
    /// Safe read-only inspection, query, or build/test verification commands.
    SafeReadOnly,
    /// Commands that mutate files, invoke network, or change system state.
    SensitiveOrExternal,
}

/// Returns true if the command is a recognized safe read-only or inspection command
/// without command chaining, redirection, or command substitution.
pub fn is_command_safe_read_only(cmd: &str) -> bool {
    matches!(classify_command(cmd), CommandTier::SafeReadOnly)
}

/// Classifies a shell command into safety tiers for granular execution approval.
pub fn classify_command(cmd: &str) -> CommandTier {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return CommandTier::SensitiveOrExternal;
    }

    // Guard against shell injection, command chaining, and file output redirection.
    // Metacharacters: ;, &&, ||, |, >, >>, <, $(, `, \n, \r
    let has_chaining_or_redirect = trimmed.chars().enumerate().any(|(i, c)| match c {
        ';' | '\n' | '\r' | '`' | '>' | '<' => true,
        '&' => trimmed.as_bytes().get(i + 1) == Some(&b'&'),
        '|' => true,
        '$' => trimmed.as_bytes().get(i + 1) == Some(&b'('),
        _ => false,
    });

    if has_chaining_or_redirect {
        return CommandTier::SensitiveOrExternal;
    }

    // Allowed base command prefixes for read-only / build verification
    const SAFE_PREFIXES: &[&str] = &[
        "cargo check",
        "cargo test",
        "cargo clippy",
        "cargo metadata",
        "cargo --version",
        "cargo version",
        "git status",
        "git diff",
        "git log",
        "git show",
        "git branch",
        "git rev-parse",
        "git describe",
        "git config --get",
        "pwd",
        "ls",
        "cat",
        "head",
        "tail",
        "wc",
        "grep",
        "rg",
        "find",
        "file",
        "which",
        "echo",
    ];

    for prefix in SAFE_PREFIXES {
        if trimmed == *prefix {
            return CommandTier::SafeReadOnly;
        }
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if rest.starts_with(' ') || rest.starts_with('\t') {
                return CommandTier::SafeReadOnly;
            }
        }
    }

    CommandTier::SensitiveOrExternal
}

/// The tool list as the model sees it (name, description, JSON Schema).
pub fn specs() -> Vec<(&'static str, &'static str, Value)> {
    let path = json!({"type": "string", "description": "Path relative to the workspace root."});
    vec![
        (
            "list_dir",
            "List the entries of a directory in the workspace.",
            json!({"type": "object", "properties": {"path": path}, "required": ["path"]}),
        ),
        (
            "read_file",
            "Read a UTF-8 text file from the workspace.",
            json!({"type": "object", "properties": {"path": path}, "required": ["path"]}),
        ),
        (
            "edit_file",
            "Replace exact text in a file. `target` must match existing file content exactly and appear once.",
            json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "target": {"type": "string", "description": "Exact text to find and replace."},
                    "replacement": {"type": "string", "description": "New text to insert in place of target."}
                },
                "required": ["path", "target", "replacement"]
            }),
        ),
        (
            "write_file",
            "Create or overwrite a text file in the workspace, creating parent directories.",
            json!({"type": "object", "properties": {"path": path, "content": {"type": "string"}},
                   "required": ["path", "content"]}),
        ),
        (
            "run",
            "Run a shell command with the workspace as the working directory. Returns exit code, stdout and stderr.",
            json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}),
        ),
    ]
}

pub async fn call(workspace: &Path, name: &str, input: &Value) -> Outcome {
    let arg = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let r = match name {
        "list_dir" => list_dir(workspace, &arg("path")),
        "read_file" => read_file(workspace, &arg("path")),
        "edit_file" => edit_file(workspace, &arg("path"), &arg("target"), &arg("replacement")),
        "write_file" => write_file(workspace, &arg("path"), &arg("content")),
        "run" => run(workspace, &arg("command")).await,
        other => Err(format!("no such tool: {other}")),
    };
    match r {
        Ok(output) => Outcome { output: cap(compact_successful_output(output)), is_error: false },
        Err(output) => Outcome { output: cap(output), is_error: true },
    }
}

pub fn compact_successful_output(s: String) -> String {
    if s.len() <= 2048 {
        return s;
    }
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() > 30 {
        let head = lines[..10].join("\n");
        let tail = lines[lines.len() - 15..].join("\n");
        let omitted = lines.len() - 25;
        format!("{head}\n\n[... {omitted} lines of output omitted for brevity ...]\n\n{tail}")
    } else {
        s
    }
}

fn cap(mut s: String) -> String {
    if s.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n[truncated]");
    }
    s
}

/// `rel` resolved inside `workspace` (already canonical), or why not.
/// The nearest existing ancestor is canonicalized, so a symlink anywhere on
/// the path that points out is caught; what does not exist yet cannot be a
/// symlink, and has no `..` to climb with.
// ponytail: check-then-use, so a symlink swapped in between the check and the
// open escapes. Fine while one agent owns its workspace; openat2/RESOLVE_BENEATH
// (or cap-std) if workspaces are ever shared with untrusted writers.
pub fn confine(workspace: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("{rel}: must be relative to the workspace, without `..`"));
    }
    let full = workspace.join(p);
    let mut probe = full.as_path();
    while !probe.exists() {
        probe = probe.parent().ok_or_else(|| format!("{rel}: no existing ancestor"))?;
    }
    let real = probe.canonicalize().map_err(|e| format!("{rel}: {e}"))?;
    if !real.starts_with(workspace) {
        return Err(format!("{rel}: resolves outside the workspace"));
    }
    Ok(full)
}

/// The workspace-relative path a file tool's `input` names, normalized, and where it is: `None`
/// when it may not be touched.
pub fn target(workspace: &Path, input: &Value) -> Option<(String, PathBuf)> {
    let full = confine(workspace, input.get("path")?.as_str()?).ok()?;
    let rel: PathBuf = full.strip_prefix(workspace).ok()?.components().collect();
    Some((rel.to_str()?.to_string(), full))
}

fn list_dir(ws: &Path, rel: &str) -> Result<String, String> {
    let dir = confine(ws, if rel.is_empty() { "." } else { rel })?;
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{rel}: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| {
            let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            format!("{}{}", e.file_name().to_string_lossy(), if dir { "/" } else { "" })
        })
        .collect();
    names.sort();
    Ok(names.join("\n"))
}

fn read_file(ws: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(confine(ws, rel)?).map_err(|e| format!("{rel}: {e}"))
}

fn write_file(ws: &Path, rel: &str, content: &str) -> Result<String, String> {
    let path = confine(ws, rel)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{rel}: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("{rel}: {e}"))?;
    Ok(format!("wrote {} bytes to {rel}", content.len()))
}

/// Locates `target` in `content`, using exact matching first and gracefully falling back to
/// whitespace/indentation-normalized line matching to eliminate common model editing failures.
pub fn locate_target(content: &str, target: &str) -> Result<(usize, usize), String> {
    if target.is_empty() {
        return Err("target string cannot be empty".into());
    }

    // 1. Fast path: exact match
    let exact_matches: Vec<_> = content.match_indices(target).collect();
    if exact_matches.len() == 1 {
        let (start, _) = exact_matches[0];
        return Ok((start, start + target.len()));
    }
    if exact_matches.len() > 1 {
        return Err(format!(
            "target string appears {} times; target must be unique (include surrounding lines for context)",
            exact_matches.len()
        ));
    }

    // 2. Trailing whitespace normalized line matching
    let target_lines: Vec<&str> = target.lines().map(str::trim_end).collect();
    if target_lines.is_empty() {
        return Err("target string not found".into());
    }

    let mut line_spans: Vec<(usize, usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in content.split('\n') {
        let end = offset + line.len();
        line_spans.push((offset, end, line.trim_end()));
        offset = end + 1;
    }

    let mut matches = Vec::new();
    let n = target_lines.len();
    if line_spans.len() >= n {
        for i in 0..=(line_spans.len() - n) {
            let window_matches = (0..n).all(|j| line_spans[i + j].2 == target_lines[j]);
            if window_matches {
                let start_byte = line_spans[i].0;
                let end_byte = line_spans[i + n - 1].1;
                matches.push((start_byte, end_byte));
            }
        }
    }

    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    if matches.len() > 1 {
        return Err(format!(
            "target string appears {} times with whitespace normalization; must be unique",
            matches.len()
        ));
    }

    // 3. Trimmed (indentation-tolerant) matching
    let target_trimmed: Vec<&str> =
        target.lines().map(str::trim).filter(|s| !s.is_empty()).collect();
    if !target_trimmed.is_empty() && line_spans.len() >= target_trimmed.len() {
        let mut trimmed_matches = Vec::new();
        let m = target_trimmed.len();
        for i in 0..=(line_spans.len() - m) {
            let window_matches = (0..m).all(|j| line_spans[i + j].2.trim() == target_trimmed[j]);
            if window_matches {
                let start_byte = line_spans[i].0;
                let end_byte = line_spans[i + m - 1].1;
                trimmed_matches.push((start_byte, end_byte));
            }
        }
        if trimmed_matches.len() == 1 {
            return Ok(trimmed_matches[0]);
        }
        if trimmed_matches.len() > 1 {
            return Err(format!(
                "target string appears {} times with indentation normalization; must be unique",
                trimmed_matches.len()
            ));
        }
    }

    Err("target string not found".into())
}

fn edit_file(ws: &Path, rel: &str, target: &str, replacement: &str) -> Result<String, String> {
    if target.is_empty() {
        return Err(format!("{rel}: target string cannot be empty"));
    }
    let path = confine(ws, rel)?;
    let content = std::fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))?;
    let (start, end) = locate_target(&content, target).map_err(|e| format!("{rel}: {e}"))?;
    let mut new_content = String::with_capacity(content.len() - (end - start) + replacement.len());
    new_content.push_str(&content[..start]);
    new_content.push_str(replacement);
    new_content.push_str(&content[end..]);
    std::fs::write(&path, &new_content).map_err(|e| format!("{rel}: {e}"))?;
    Ok(format!("replaced 1 occurrence in {rel}"))
}

async fn run(ws: &Path, command: &str) -> Result<String, String> {
    if command.trim().is_empty() {
        return Err("empty command".into());
    }
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(ws)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(RUN_TIMEOUT, child)
        .await
        .map_err(|_| format!("timed out after {}s", RUN_TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())?;
    let text = format!(
        "exit: {}\nstdout:\n{}\nstderr:\n{}",
        out.status.code().map_or("signal".to_string(), |c| c.to_string()),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().canonicalize().unwrap();
        (d, p)
    }

    #[test]
    fn a_path_cannot_leave_the_workspace() {
        let (_d, ws) = ws();
        assert!(confine(&ws, "/etc/passwd").is_err());
        assert!(confine(&ws, "../x").is_err());
        assert!(confine(&ws, "a/../../x").is_err());
        assert!(confine(&ws, "new/dir/file.txt").is_ok());
        assert!(confine(&ws, ".").is_ok());
    }

    #[test]
    fn a_symlink_out_of_the_workspace_is_refused() {
        let (_d, ws) = ws();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.join("escape")).unwrap();
        assert!(confine(&ws, "escape/secret").is_err());
        assert!(write_file(&ws, "escape/secret", "x").is_err());
        assert!(!outside.path().join("secret").exists());
    }

    #[tokio::test]
    async fn the_tools_round_trip_a_file() {
        let (_d, ws) = ws();
        let w =
            call(&ws, "write_file", &json!({"path": "src/a.txt", "content": "hi\nworld\n"})).await;
        assert!(!w.is_error, "{}", w.output);
        assert_eq!(
            call(&ws, "read_file", &json!({"path": "src/a.txt"})).await.output,
            "hi\nworld\n"
        );
        assert_eq!(call(&ws, "list_dir", &json!({"path": "."})).await.output, "src/");

        let e = call(
            &ws,
            "edit_file",
            &json!({"path": "src/a.txt", "target": "world", "replacement": "there"}),
        )
        .await;
        assert!(!e.is_error, "{}", e.output);
        assert_eq!(
            call(&ws, "read_file", &json!({"path": "src/a.txt"})).await.output,
            "hi\nthere\n"
        );

        let miss = call(
            &ws,
            "edit_file",
            &json!({"path": "src/a.txt", "target": "not-here", "replacement": "x"}),
        )
        .await;
        assert!(miss.is_error && miss.output.contains("not found"));

        let dup = call(
            &ws,
            "edit_file",
            &json!({"path": "src/a.txt", "target": "\n", "replacement": " "}),
        )
        .await;
        assert!(dup.is_error && dup.output.contains("appears 2 times"));

        let r = call(&ws, "run", &json!({"command": "cat src/a.txt; exit 3"})).await;
        assert!(r.is_error && r.output.contains("exit: 3") && r.output.contains("hi\nthere\n"));
        assert!(call(&ws, "nope", &json!({})).await.is_error);
    }

    #[test]
    fn command_safety_classification_distinguishes_safe_read_only_and_injections() {
        assert_eq!(classify_command("cargo check"), CommandTier::SafeReadOnly);
        assert_eq!(classify_command("cargo test -p holon-agentd"), CommandTier::SafeReadOnly);
        assert_eq!(classify_command("git status"), CommandTier::SafeReadOnly);
        assert_eq!(classify_command("git diff HEAD~1"), CommandTier::SafeReadOnly);
        assert_eq!(classify_command("ls -la src/"), CommandTier::SafeReadOnly);
        assert_eq!(classify_command("cat Cargo.toml"), CommandTier::SafeReadOnly);
        assert_eq!(classify_command("grep -rn 'todo' src/"), CommandTier::SafeReadOnly);

        // Injections and dangerous metacharacters
        assert_eq!(classify_command("cargo check; rm -rf /"), CommandTier::SensitiveOrExternal);
        assert_eq!(
            classify_command("git status && curl evil.com"),
            CommandTier::SensitiveOrExternal
        );
        assert_eq!(classify_command("cat Cargo.toml > evil.txt"), CommandTier::SensitiveOrExternal);
        assert_eq!(classify_command("echo `whoami`"), CommandTier::SensitiveOrExternal);
        assert_eq!(classify_command("echo $(id)"), CommandTier::SensitiveOrExternal);
        assert_eq!(classify_command("git status | bash"), CommandTier::SensitiveOrExternal);
        assert_eq!(classify_command("rm -rf target/"), CommandTier::SensitiveOrExternal);
        assert_eq!(classify_command("git push origin main"), CommandTier::SensitiveOrExternal);
        assert_eq!(classify_command(""), CommandTier::SensitiveOrExternal);
    }

    #[test]
    fn locate_target_matches_exact_and_tolerates_trailing_whitespace_and_indentation() {
        let content = "fn main() {\n    let a = 1;\n    let b = 2;\n}\n";

        // Target with extra trailing spaces that don't exist in file
        let (s, e) = locate_target(content, "    let a = 1;   ").unwrap();
        assert_eq!(&content[s..e], "    let a = 1;");

        // Indentation discrepancy in multi-line target
        let (s, e) = locate_target(content, "let a = 1;\nlet b = 2;").unwrap();
        assert_eq!(&content[s..e], "    let a = 1;\n    let b = 2;");

        // Unique error on duplicates
        let dup = "x = 1;\nx = 1;\n";
        assert!(locate_target(dup, "x = 1;").unwrap_err().contains("appears 2 times"));

        // Not found
        assert!(locate_target(content, "non_existent").unwrap_err().contains("not found"));
    }

    #[test]
    fn compact_successful_output_trims_large_passing_logs() {
        let mut long_log = String::new();
        for i in 0..100 {
            long_log.push_str(&format!("test row {i} passed successfully in 0.01s\n"));
        }
        let compacted = compact_successful_output(long_log);
        assert!(compacted.contains("omitted for brevity"));
        assert!(compacted.contains("test row 0 passed"));
        assert!(compacted.contains("test row 99 passed"));
    }
}
