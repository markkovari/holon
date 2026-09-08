//! `comp-cron` — read the machine's crontab, for a component that has none
//!
//! ## Why this is a process and not a component
//!
//! Reading crontab needs `crontab -l`, and a `wasm32-wasip2` guest cannot spawn
//! a process (ADR-0095). That is the sandbox working rather than a gap to
//! route around, so the part that needs an operating system is native and is
//! reached over HTTP exactly like the watcher and the transcoder are.
//!
//! `components/system-cron` is the component side: it holds the WIT contract
//! and dials this. Nothing here knows what a goal is.
//!
//! ## No allow-list here
//!
//! `comp-fswatch` and `comp-ffmpeg` take `--allow-path` because a request
//! names a filesystem path a model chose. This daemon reads exactly one
//! thing — the crontab of the user running it — so there is nothing for a
//! request to name and nothing for an allow-list to scope.
//!
//! ## No crontab installed is not a failure
//!
//! `crontab -l` exits nonzero and writes "no crontab for <user>" to stderr
//! when nobody has installed one. That is an empty job list, not
//! `unavailable`: `unavailable` is reserved for the daemon failing to run
//! `crontab` at all (binary missing, spawn failed).
//!
//!   comp-cron --addr 127.0.0.1:8008

use anyhow::Result;
use axum::{routing::post, Json, Router};
use clap::Parser;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-cron", about = "Native daemon for system-cron")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8008")]
    addr: String,
}

#[derive(Serialize)]
struct Job {
    schedule: String,
    command: String,
}

/// One 5-field cron line -> a job. Blank lines and comments are not jobs.
///
/// `0 3 * * * /usr/bin/backup.sh` -> schedule `"0 3 * * *"`, command
/// `"/usr/bin/backup.sh"`.
fn parse_line(line: &str) -> Option<Job> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 6 {
        return None;
    }
    Some(Job { schedule: fields[..5].join(" "), command: fields[5..].join(" ") })
}

fn parse_crontab(output: &str) -> Vec<Job> {
    output.lines().filter_map(parse_line).collect()
}

async fn list_jobs() -> Json<Value> {
    let out = tokio::process::Command::new("crontab").arg("-l").output().await;
    let out = match out {
        Ok(o) => o,
        // The binary itself could not be found or spawned — that is the only
        // thing this reports as unavailable.
        Err(e) => {
            return Json(json!({ "error": "unavailable", "detail": format!("failed to run crontab: {e}") }))
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // "no crontab for <user>" is the standard message crontab prints when
        // nobody has installed one — that is an empty list, not a failure.
        if stderr.to_lowercase().contains("no crontab for") || out.stdout.is_empty() {
            return Json(json!({ "jobs": [] }));
        }
        return Json(json!({ "error": "unavailable", "detail": stderr.trim() }));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let jobs = parse_crontab(&stdout);
    Json(json!({ "jobs": jobs.iter().map(|j| json!({"schedule": j.schedule, "command": j.command})).collect::<Vec<_>>() }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("comp-cron: listening on http://{}", args.addr);
    let app = Router::new().route("/list-jobs", post(list_jobs));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_line_becomes_a_job() {
        let job = parse_line("0 3 * * * /usr/bin/backup.sh").expect("parses");
        assert_eq!(job.schedule, "0 3 * * *");
        assert_eq!(job.command, "/usr/bin/backup.sh");
    }

    #[test]
    fn blank_lines_and_comments_are_skipped() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        assert!(parse_line("# a comment").is_none());
        assert!(parse_line("  # indented comment").is_none());
    }

    #[test]
    fn extra_whitespace_between_fields_does_not_change_the_result() {
        let job = parse_line("  0   3  *  *   *    /usr/bin/backup.sh --flag  ").expect("parses");
        assert_eq!(job.schedule, "0 3 * * *");
        assert_eq!(job.command, "/usr/bin/backup.sh --flag");
    }

    #[test]
    fn a_full_crontab_is_parsed_line_by_line() {
        let text = "# comment\n\n0 3 * * * /usr/bin/backup.sh\n*/5 * * * * /usr/bin/ping.sh\n";
        let jobs = parse_crontab(text);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].command, "/usr/bin/backup.sh");
        assert_eq!(jobs[1].schedule, "*/5 * * * *");
    }
}
