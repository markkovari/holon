//! What a goal's app is made of, and how much of it the run had to write.
//!
//!     comp-reuse-ratio .comp/goals/triage-assist.toml [...]
//!
//! Two numbers per app, from the capability graph (`comp-capgraph`, ADR-0091) —
//! rows a query can join against, kept current by the build — rather than by
//! shelling out to `comp-plug`/`wasm-tools` and regexing their output or a
//! `.wit` file per invocation:
//!
//!   COMPOSITION   the other artifacts this app's `carries` edges name — the
//!                 same wiring `comp-plug` computes, already projected.
//!   CAPABILITIES  the interfaces this artifact's `imports` edges name — what
//!                 the compiled binary actually calls.
//!
//! What this trades away: the graph's `imports` edges are derived from the
//! BUILT artifact (same as the old `wasm-tools` reading), not from the `.wit`
//! world source, so there is no record here of a capability a world OFFERS but
//! nothing calls — the old "world offers N, UNUSED: ..." half of the report.
//! That is a real capability this version does not have, not an oversight;
//! restoring it means projecting the world's declared surface into the graph
//! too, which is a `comp-capgraph` schema change, not one to make from here.
//!
//! Also: the graph reflects whatever `comp-capgraph`'s last run projected, not
//! necessarily this exact tree — unlike the artifact-derived path this
//! replaces, which read the measured tree's own build. `REUSE_ROOT` still
//! selects which tree's goal spec and written files are read (the stub check
//! below), just no longer which tree's build the composition numbers come from.
//!
//!     comp-reuse-ratio --surreal-url http://malna.tail3a9c.ts.net:8000 <goal>...

use clap::Parser;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn measured_root(repo: &Path) -> PathBuf {
    std::env::var("REUSE_ROOT").map(PathBuf::from).unwrap_or_else(|_| repo.to_path_buf())
}

#[derive(Parser)]
#[command(name = "comp-reuse-ratio", about = "What a goal's app is made of, and how much of it the run had to write")]
struct Args {
    /// Goal spec paths, e.g. `.comp/goals/triage-assist.toml`.
    goals: Vec<String>,
    /// The capability graph's SurrealDB endpoint (comp-capgraph, ADR-0091).
    #[arg(long, default_value = "http://malna.tail3a9c.ts.net:8000")]
    surreal_url: String,
    /// Password for that database, as a FILE path — same convention as
    /// `comp-goalrun --surreal-password`. Absent means unauthenticated.
    #[arg(long)]
    surreal_password: Option<PathBuf>,
}

/// A thin client over SurrealDB's `/sql` HTTP endpoint — the same wire
/// protocol `reconciler/src/trace.rs` already uses, at the `comp`/`goalmemory`
/// coordinates `comp-capgraph --format surql` projects into.
struct Surreal {
    url: String,
    password: Option<String>,
    client: reqwest::blocking::Client,
}

impl Surreal {
    fn new(url: String, password: Option<String>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { url: url.trim_end_matches('/').to_string(), password, client }
    }

    /// Run one SurrealQL statement and return its `result` array. Empty on any
    /// failure (unreachable database, bad auth, a rejected statement) — a
    /// measurement tool that cannot reach its data source reports nothing for
    /// that app rather than crashing the whole run over one.
    fn query(&self, surql: &str) -> Vec<Value> {
        let mut req = self.client.post(format!("{}/sql", self.url));
        if let Some(p) = &self.password {
            req = req.basic_auth("root", Some(p));
        }
        let Ok(resp) = req
            .header("accept", "application/json")
            .header("surreal-ns", "comp")
            .header("surreal-db", "goalmemory")
            .body(surql.to_string())
            .send()
        else {
            return Vec::new();
        };
        if !resp.status().is_success() {
            return Vec::new();
        }
        let Ok(text) = resp.text() else { return Vec::new() };
        let Ok(statements) = serde_json::from_str::<Vec<Value>>(&text) else { return Vec::new() };
        statements
            .into_iter()
            .last()
            .and_then(|s| s.get("result").cloned())
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
    }
}

/// A JSON string literal, so a crate name cannot carry SurrealQL syntax
/// (matches `comp-capgraph`'s own `lit()`, ADR-0080's reasoning).
fn lit(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

/// A SurrealDB record id in `comp-capgraph`'s escaped form — same escaping, so
/// ids always agree with what the projection wrote.
fn rid(table: &str, id: &str) -> String {
    format!("{table}:⟨{}⟩", id.replace('⟩', ""))
}

fn first_array<'a>(rows: &'a [Value], field: &str) -> Vec<&'a str> {
    rows.first()
        .and_then(|row| row.get(field))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The other artifacts wired into the app whose domain component is
/// `crate_name` — `comp-capgraph`'s `carries` edges, minus the root itself
/// (which `carries` includes; see `capgraph.rs`'s own comment on why).
fn wired_peers(surreal: &Surreal, crate_name: &str) -> Vec<String> {
    let rows = surreal.query(&format!(
        "SELECT ->carries->artifact.name AS parts FROM app WHERE root = {};",
        lit(crate_name)
    ));
    first_array(&rows, "parts").into_iter().filter(|s| *s != crate_name).map(String::from).collect()
}

/// The interfaces `crate_name`'s compiled artifact imports — `comp-capgraph`'s
/// `imports` edges, read out of the binary the same way `wasm-tools` did.
fn imported_interfaces(surreal: &Surreal, crate_name: &str) -> Vec<String> {
    let rows =
        surreal.query(&format!("SELECT ->imports->interface.name AS imported FROM {};", rid("artifact", crate_name)));
    first_array(&rows, "imported").into_iter().map(String::from).collect()
}

struct Row {
    reused: usize,
    written: usize,
}

fn report(root: &Path, surreal: &Surreal, goal_path: &str) -> Option<Row> {
    let text = std::fs::read_to_string(goal_path).ok()?;
    let goal: toml::Value = toml::from_str(&text).ok()?;
    let title = goal.get("title").and_then(|t| t.as_str()).unwrap_or(goal_path).to_string();

    let written_files: Vec<String> = goal
        .get("part")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("writable").and_then(|w| w.as_array()))
        .flatten()
        .filter_map(|w| w.as_str())
        .filter(|w| w.ends_with(".rs"))
        .map(String::from)
        .collect();

    let crate_name = written_files.iter().find_map(|w| {
        let parts: Vec<&str> = w.split('/').collect();
        (parts.len() > 2 && parts[0] == "components").then(|| parts[1].to_string())
    })?;

    // A stub tree would report a 99% ratio against sixteen lines of `not_implemented`,
    // which is true and meaningless. Say so rather than print it as a result.
    let stub_count = written_files
        .iter()
        .filter(|w| std::fs::read_to_string(root.join(w)).is_ok_and(|c| c.contains(r#"501, "not_implemented""#)))
        .count();
    if stub_count > 0 {
        println!("\n=== {title}");
        println!(
            "    SKIPPED — {stub_count} of {} written file(s) are still stubs in this tree.\n    Measure a landed branch instead: REUSE_ROOT=<worktree> comp-reuse-ratio {goal_path}",
            written_files.len()
        );
        return None;
    }

    let wired = wired_peers(surreal, &crate_name);
    let reused = wired.len();
    let written = 1; // the goal writes exactly its own crate
    let called = imported_interfaces(surreal, &crate_name);

    let ratio = reused as f64 / (reused + written) as f64;
    println!("\n=== {title}");
    println!("    crate: {crate_name}");
    println!("  COMPOSITION  {} component(s) wired in: {}", wired.len(), wired.join(", "));
    println!("  COMPONENTS   reused {reused} component(s)  ·  written {written} component(s)");
    println!(
        "               reuse ratio {:.1}%  — {}x more existing components than authored",
        ratio * 100.0,
        reused / written
    );
    println!("  CAPABILITIES artifact imports {} interface(s): {}", called.len(), called.join(", "));
    Some(Row { reused, written })
}

fn main() {
    let args = Args::parse();
    let root = measured_root(&repo());
    let password = args
        .surreal_password
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string());
    let surreal = Surreal::new(args.surreal_url, password);

    let rows: Vec<Row> = args.goals.iter().filter_map(|g| report(&root, &surreal, g)).collect();
    if rows.len() > 1 {
        let (tr, tw) = rows.iter().fold((0usize, 0usize), |(a, b), r| (a + r.reused, b + r.written));
        println!(
            "\n=== all {} app(s): reused {tr} components, written {tw} components, ratio {:.1}%",
            rows.len(),
            100.0 * tr as f64 / (tr + tw) as f64
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_escapes_a_quote_so_a_crate_name_cannot_carry_surql_syntax() {
        assert_eq!(lit(r#"a"; DROP TABLE app; --"#), r#""a\"; DROP TABLE app; --""#);
    }

    #[test]
    fn rid_strips_a_stray_closing_angle_bracket_rather_than_letting_it_close_early() {
        assert_eq!(rid("artifact", "weird⟩name"), "artifact:⟨weirdname⟩");
    }

    #[test]
    fn rid_is_stable_for_an_ordinary_name() {
        assert_eq!(rid("app", "flags"), "app:⟨flags⟩");
    }

    #[test]
    fn first_array_reads_the_named_field_of_the_first_row() {
        let rows = serde_json::json!([{"parts": ["a", "b"]}]);
        assert_eq!(first_array(rows.as_array().unwrap(), "parts"), vec!["a", "b"]);
    }

    #[test]
    fn first_array_is_empty_for_no_rows() {
        let rows: Vec<Value> = vec![];
        assert!(first_array(&rows, "parts").is_empty());
    }

    #[test]
    fn first_array_is_empty_when_the_field_is_missing() {
        let rows = serde_json::json!([{}]);
        assert!(first_array(rows.as_array().unwrap(), "parts").is_empty());
    }
}
