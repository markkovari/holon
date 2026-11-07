//! `propose_pr`: a real pull request. A branch off `origin/<base>` in a worktree of `--pr-repo`,
//! the files, one commit, pushed, and opened with `gh`; what comes back is what GitHub answered.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// Where proposals go: a git checkout whose `origin` takes the branch, and the `gh` that opens it.
#[derive(Clone, Debug)]
pub struct Forge {
    pub repo: PathBuf,
    pub gh: PathBuf,
}

pub struct Proposal {
    pub agent: String,
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
    /// (path in the repository, content)
    pub files: Vec<(String, Vec<u8>)>,
}

pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 100
        && !b.starts_with(['-', '/', '.'])
        && !b.ends_with(['/', '.'])
        && !b.contains("..")
        && !b.contains("//")
        && b.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.".contains(c))
}

/// Relative, inside the repository, and not its `.git`.
pub fn valid_path(p: &str) -> bool {
    let path = Path::new(p);
    !p.is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && path.components().next().is_some_and(|c| c.as_os_str() != ".git")
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{}: {}", program(cmd), err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn program(cmd: &Command) -> String {
    let args: Vec<_> = cmd.get_args().take(2).map(|a| a.to_string_lossy().into_owned()).collect();
    format!("{} {}", cmd.get_program().to_string_lossy(), args.join(" "))
}

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    c
}

/// `work` is a scratch directory for the worktree; it is removed whatever happens.
pub fn open(forge: &Forge, work: &Path, p: &Proposal) -> Result<Value, String> {
    if !valid_branch(&p.branch) || !valid_branch(&p.base) {
        return Err(format!("`{}` onto `{}`: not a branch name", p.branch, p.base));
    }
    if p.title.trim().is_empty() {
        return Err("a pull request needs a title".into());
    }
    if p.files.is_empty() {
        return Err("nothing to propose: give `changes`, or the `tool` you built".into());
    }
    if let Some((bad, _)) = p.files.iter().find(|(path, _)| !valid_path(path)) {
        return Err(format!("`{bad}` is not a path inside the repository"));
    }
    let dir = work.join(p.branch.replace('/', "-"));
    let _ = run(git(&forge.repo).args(["worktree", "remove", "--force"]).arg(&dir));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    run(git(&forge.repo).args(["fetch", "--quiet", "origin", &p.base]))?;
    let start = format!("origin/{}", p.base);
    run(git(&forge.repo)
        .args(["worktree", "add", "--quiet", "-B", &p.branch])
        .arg(&dir)
        .arg(&start))?;
    let opened = commit_push_open(forge, &dir, p);
    let _ = run(git(&forge.repo).args(["worktree", "remove", "--force"]).arg(&dir));
    opened
}

fn commit_push_open(forge: &Forge, dir: &Path, p: &Proposal) -> Result<Value, String> {
    for (path, content) in &p.files {
        let to = dir.join(path);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&to, content).map_err(|e| format!("{path}: {e}"))?;
    }
    run(git(dir).args(["add", "-A"]))?;
    let author = format!("agent {} <agent-{}@holon.local>", p.agent, p.agent);
    run(git(dir)
        .args(["-c", "commit.gpgsign=false", "commit", "--quiet", "--author", &author])
        .args(["-m", &p.title, "-m", &p.body]))?;
    let commit = run(git(dir).args(["rev-parse", "HEAD"]))?;
    run(git(dir).args(["push", "--quiet", "--force-with-lease", "-u", "origin", &p.branch]))?;
    let url = run(Command::new(&forge.gh).current_dir(dir).args([
        "pr", "create", "--base", &p.base, "--head", &p.branch, "--title", &p.title, "--body",
        &p.body,
    ]))?;
    let url = url.lines().last().unwrap_or_default().to_string();
    let number = url.rsplit('/').next().and_then(|n| n.parse::<u64>().ok());
    Ok(json!({
        "number": number,
        "url": url,
        "commit": commit,
        "branch": p.branch,
        "base": p.base,
        "title": p.title,
        "files": p.files.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(),
        "author_agent": p.agent,
        "status": "opened",
    }))
}

/// A built tool's component directory, as the files of `components/<tool>-mbt/`: what
/// `tool_build` compiled, without its build output.
pub fn tool_files(
    forge_dir: &Path,
    agent: &str,
    tool: &str,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let root = forge_dir.join(format!("{agent}-{tool}"));
    if !root.join("tool/tool.mbt").exists() {
        return Err(format!("you have built no tool named `{tool}`"));
    }
    let mut out = Vec::new();
    collect(&root, &root, &format!("components/{}-mbt", tool.replace('_', "-")), &mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

fn collect(
    root: &Path,
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(String, Vec<u8>)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if name == "_build" || name == "target" || name == ".mooncakes" || name == "text" {
            continue;
        }
        if name.ends_with(".wasm") || (dir == root && name == "tool.mbt") {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, prefix, out)?;
        } else {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            out.push((format!("{prefix}/{rel}"), std::fs::read(&path)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_and_paths_stay_inside_the_repository() {
        assert!(valid_branch("tool/prime-count") && valid_branch("feat/a_b.c"));
        for b in ["", "-x", "/x", "a..b", "a b", "a//b", "a/", "x;rm"] {
            assert!(!valid_branch(b), "{b}");
        }
        assert!(valid_path("components/x-mbt/tool/tool.mbt"));
        for p in ["", "/etc/passwd", "../x", "a/../../x", ".git/config", "./x"] {
            assert!(!valid_path(p), "{p}");
        }
    }
}
