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
        "write_file" => write_file(workspace, &arg("path"), &arg("content")),
        "run" => run(workspace, &arg("command")).await,
        other => Err(format!("no such tool: {other}")),
    };
    match r {
        Ok(output) => Outcome { output: cap(output), is_error: false },
        Err(output) => Outcome { output: cap(output), is_error: true },
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
fn confine(workspace: &Path, rel: &str) -> Result<PathBuf, String> {
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
        let w = call(&ws, "write_file", &json!({"path": "src/a.txt", "content": "hi"})).await;
        assert!(!w.is_error, "{}", w.output);
        assert_eq!(call(&ws, "read_file", &json!({"path": "src/a.txt"})).await.output, "hi");
        assert_eq!(call(&ws, "list_dir", &json!({"path": "."})).await.output, "src/");
        let r = call(&ws, "run", &json!({"command": "cat src/a.txt; exit 3"})).await;
        assert!(r.is_error && r.output.contains("exit: 3") && r.output.contains("hi"));
        assert!(call(&ws, "nope", &json!({})).await.is_error);
    }
}
