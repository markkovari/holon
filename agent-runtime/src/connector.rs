//! A tool configured not coded. The call's arguments go as JSON on stdin to a local program, as a
//! POST body to a `url`, or over NATS to a component on the lattice exporting `holon:tool/tool`
//! (`wit/tool/tool.wit`, served by comp-host over wRPC, ADR-0028).

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Exec {
    /// Program and fixed arguments; never run through a shell.
    #[serde(default)]
    pub command: Vec<String>,
    /// Instead of `command`: POST the arguments here, and the response body is the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Instead of `command`: call `holon:tool/tool.call` on this lattice instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrpc: Option<Wrpc>,
    /// A hint for the model, e.g. `{"from": "<ISO date>", "to": "<ISO date>"}`.
    #[serde(default)]
    pub args: String,
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

/// A tool component on the lattice. NATS is at `HOLON_NATS_URL` (default `nats://127.0.0.1:4222`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Wrpc {
    pub lattice: String,
    /// `<tenant>/<app>/<component>`, as comp-host names the instance it started.
    pub instance: String,
}

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

pub fn run(exec: &Exec, args: &Value, workspace: Option<&Path>) -> Result<String, String> {
    if let Some(url) = &exec.url {
        return post(url, args, exec.timeout_secs);
    }
    if let Some(w) = &exec.wrpc {
        return invoke(w, args, exec.timeout_secs);
    }
    let (prog, fixed) = exec.command.split_first().ok_or("connector has no command")?;
    let mut child = Command::new(prog)
        .args(fixed)
        .env_clear()
        .envs(
            std::env::vars()
                .filter(|(k, _)| matches!(k.as_str(), "PATH" | "HOME" | "LANG" | "TMPDIR")),
        )
        .envs(workspace.map(|w| ("HOLON_WORKSPACE", w)))
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

fn post(url: &str, args: &Value, timeout_secs: u64) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("{url}: a connector url must be http(s)"));
    }
    let timeout = timeout_secs.max(1);
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| e.to_string())?
        .post(url)
        .json(args)
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                format!("timed out after {timeout}s")
            } else {
                format!("{url}: {e}")
            }
        })?;
    let status = resp.status();
    let mut buf = Vec::new();
    let _ = resp.take(MAX_OUT as u64).read_to_end(&mut buf);
    let body = String::from_utf8_lossy(&buf).trim().to_string();
    match (status.is_success(), body.is_empty()) {
        (true, true) => Ok("(no output)".into()),
        (true, false) => Ok(body),
        (false, true) => Err(format!("{url} answered {status}")),
        (false, false) => Err(body),
    }
}

fn invoke(w: &Wrpc, args: &Value, timeout_secs: u64) -> Result<String, String> {
    use wrpc_transport::InvokeExt as _;
    let nats_url =
        std::env::var("HOLON_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let timeout = timeout_secs.max(1);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let call = async {
            let nats =
                async_nats::connect(&nats_url).await.map_err(|e| format!("{nats_url}: {e}"))?;
            let prefix = holon_lattice::wire::rpc_prefix(&w.lattice, &w.instance);
            let client = wrpc_transport_nats::Client::new(nats, prefix, None)
                .await
                .map_err(|e| format!("{}: {e:#}", w.instance))?;
            let (out,): (Result<String, String>,) = client
                .invoke_values_blocking(
                    None,
                    "holon:tool/tool@0.1.0",
                    "call",
                    (args.to_string(),),
                    &[[None::<usize>; 0]; 0],
                )
                .await
                .map_err(|e| format!("{}: {e:#}", w.instance))?;
            out
        };
        tokio::time::timeout(Duration::from_secs(timeout), call)
            .await
            .unwrap_or_else(|_| Err(format!("timed out after {timeout}s")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn exec(cmd: &[&str]) -> Exec {
        Exec {
            command: cmd.iter().map(|s| s.to_string()).collect(),
            url: None,
            wrpc: None,
            args: String::new(),
            required: vec![],
            sensitive: false,
            timeout_secs: 5,
        }
    }

    #[test]
    fn arguments_arrive_as_json_on_stdin_and_stdout_comes_back() {
        let out = run(&exec(&["cat"]), &json!({"from": "2026-10-06"}), None).unwrap();
        assert_eq!(out, r#"{"from":"2026-10-06"}"#);
    }

    #[test]
    fn failure_reports_stderr_a_hang_times_out_and_nothing_goes_through_a_shell() {
        assert_eq!(
            run(&exec(&["sh", "-c", "echo nope >&2; exit 3"]), &json!({}), None).unwrap_err(),
            "nope"
        );
        let mut slow = exec(&["sleep", "5"]);
        slow.timeout_secs = 1;
        assert!(run(&slow, &json!({}), None).unwrap_err().contains("timed out"));
        assert!(run(&exec(&["cat"]), &json!({"x": "; rm -rf /"}), None)
            .unwrap()
            .contains("rm -rf"));
        assert!(run(&exec(&["/nonexistent/program"]), &json!({}), None).is_err());
        let ws = std::path::Path::new("/tmp/some-agent-workspace");
        let sh = exec(&["sh", "-c", "printf %s \"${HOLON_WORKSPACE:-unset}\""]);
        assert_eq!(run(&sh, &json!({}), Some(ws)).unwrap(), "/tmp/some-agent-workspace");
        assert_eq!(run(&sh, &json!({}), None).unwrap(), "unset");
    }

    /// Answers one request with `status` and `body`, and hands back what it was sent.
    fn serve_once(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/call", l.local_addr().unwrap());
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = s.read(&mut buf) {
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if rest.len() >= len {
                        break;
                    }
                }
            }
            s.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });
        (url, h)
    }

    fn at(url: &str) -> Exec {
        Exec { url: Some(url.into()), ..exec(&[]) }
    }

    #[test]
    fn a_url_connector_posts_the_arguments_and_the_body_is_the_result() {
        let (url, h) = serve_once("200 OK", "größe ✓");
        assert_eq!(run(&at(&url), &json!({"text": "Grüße"}), None).unwrap(), "größe ✓");
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /call "), "{req}");
        assert!(req.ends_with(r#"{"text":"Grüße"}"#), "{req}");

        let (url, h) = serve_once("422 Unprocessable Entity", "text is required");
        assert_eq!(run(&at(&url), &json!({}), None).unwrap_err(), "text is required");
        h.join().unwrap();
        let (url, h) = serve_once("500 Internal Server Error", "");
        assert!(run(&at(&url), &json!({}), None).unwrap_err().contains("answered 500"));
        h.join().unwrap();

        assert!(run(&at("file:///etc/passwd"), &json!({}), None).unwrap_err().contains("http(s)"));
        assert!(run(&at("http://127.0.0.1:1/"), &json!({}), None).is_err());
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
