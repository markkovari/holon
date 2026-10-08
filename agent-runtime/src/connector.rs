//! Connectors: a tool backed by a program on this machine (a calendar, a mail client, a
//! home-automation CLI). The agent calls it like any tool; the runtime runs the program with
//! the call's arguments as JSON on stdin and hands back whatever it prints.
//!
//! A connector is configuration, not code in this repo: define it inline on an agent's
//! capability (`exec`), or once in `<state>/connectors/<name>.json` and name it in the
//! capability list. Mark it `sensitive` and every call waits for the owner's approval, the same
//! as writing a file does.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Exec {
    /// Program and fixed arguments; never run through a shell.
    pub command: Vec<String>,
    /// How the model should write the call: `{"from": "<ISO date>", "to": "<ISO date>"}`.
    #[serde(default)]
    pub args: String,
    /// Arguments a call cannot work without.
    #[serde(default)]
    pub required: Vec<String>,
    /// Needs the owner's approval for every call.
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    30
}

/// A connector defined once, in `<state>/connectors/<name>.json`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Definition {
    #[serde(default)]
    pub description: String,
    #[serde(flatten)]
    pub exec: Exec,
}

pub fn load(dir: &Path, name: &str) -> Option<Definition> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{name}.json"))).ok()?).ok()
}

const MAX_OUT: usize = 64 * 1024;

pub fn run(exec: &Exec, args: &Value) -> Result<String, String> {
    let (prog, fixed) = exec.command.split_first().ok_or("connector has no command")?;
    let mut child = Command::new(prog)
        .args(fixed)
        .env_clear()
        .envs(
            std::env::vars()
                .filter(|(k, _)| matches!(k.as_str(), "PATH" | "HOME" | "LANG" | "TMPDIR")),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(args.to_string().as_bytes());
    }
    let (out, err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let read = |r: &mut dyn Read| {
        let mut buf = Vec::new();
        let _ = r.take(MAX_OUT as u64).read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).trim().to_string()
    };
    let mut out = out;
    let reader_out = std::thread::spawn(move || read(&mut out));
    let reader_err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err.take(MAX_OUT as u64).read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).trim().to_string()
    });
    let deadline = Instant::now() + Duration::from_secs(exec.timeout_secs.max(1));
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", exec.timeout_secs));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let (stdout, stderr) =
        (reader_out.join().unwrap_or_default(), reader_err.join().unwrap_or_default());
    if status.success() {
        Ok(if stdout.is_empty() { "(no output)".into() } else { stdout })
    } else {
        Err(if stderr.is_empty() { format!("exited with {status}") } else { stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn exec(cmd: &[&str]) -> Exec {
        Exec {
            command: cmd.iter().map(|s| s.to_string()).collect(),
            args: String::new(),
            required: vec![],
            sensitive: false,
            timeout_secs: 5,
        }
    }

    #[test]
    fn arguments_arrive_as_json_on_stdin_and_stdout_comes_back() {
        let out = run(&exec(&["cat"]), &json!({"from": "2026-10-06"})).unwrap();
        assert_eq!(out, r#"{"from":"2026-10-06"}"#);
    }

    #[test]
    fn failure_reports_stderr_a_hang_times_out_and_nothing_goes_through_a_shell() {
        assert_eq!(
            run(&exec(&["sh", "-c", "echo nope >&2; exit 3"]), &json!({})).unwrap_err(),
            "nope"
        );
        let mut slow = exec(&["sleep", "5"]);
        slow.timeout_secs = 1;
        assert!(run(&slow, &json!({})).unwrap_err().contains("timed out"));
        // a hostile argument is just text on stdin, never a command
        assert!(run(&exec(&["cat"]), &json!({"x": "; rm -rf /"})).unwrap().contains("rm -rf"));
        assert!(run(&exec(&["/nonexistent/program"]), &json!({})).is_err());
    }

    #[test]
    fn definitions_load_by_safe_name_only() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("calendar_list.json"),
            r#"{"description":"Events","command":["cal","list"],"sensitive":true}"#,
        )
        .unwrap();
        let def = load(d.path(), "calendar_list").unwrap();
        assert!(def.exec.sensitive && def.exec.timeout_secs == 30 && def.description == "Events");
        assert!(load(d.path(), "../calendar_list").is_none());
        assert!(load(d.path(), "missing").is_none());
    }
}
