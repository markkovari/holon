//! Native side of components/system-cron (ADR-0095): a wasm guest cannot run `crontab -l`.

use anyhow::Result;
use axum::routing::post;
use axum::{Json, Router};
use clap::Parser;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "holon-cron", about = "Native daemon for system-cron")]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    #[arg(long, default_value = "127.0.0.1:8008")]
    addr: String,
}

#[derive(Serialize)]
struct Job {
    schedule: String,
    command: String,
}

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
        // Only a missing or unspawnable binary is `unavailable`.
        Err(e) => {
            return Json(
                json!({ "error": "unavailable", "detail": format!("failed to run crontab: {e}") }),
            )
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The standard message when nobody has installed a crontab: an empty list, not a failure.
        if stderr.to_lowercase().contains("no crontab for") || out.stdout.is_empty() {
            return Json(json!({ "jobs": [] }));
        }
        return Json(json!({ "error": "unavailable", "detail": stderr.trim() }));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let jobs = parse_crontab(&stdout);
    Json(
        json!({ "jobs": jobs.iter().map(|j| json!({"schedule": j.schedule, "command": j.command})).collect::<Vec<_>>() }),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = comp_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    comp_daemon_kit::warn_if_unauthenticated("holon-cron", &token);
    println!("holon-cron: listening on http://{}", args.addr);
    let app = Router::new()
        .route("/list-jobs", post(list_jobs))
        .layer(axum::middleware::from_fn(comp_daemon_kit::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
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
