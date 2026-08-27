//! The `.feature` files under `features/`, executed.
//!
//! Two claims, and they are different:
//!
//!   1. every feature file in this repository is VALID Gherkin — `gherkin:validate`
//!      says so, and a file it calls broken fails the build rather than sitting
//!      there being wrong;
//!   2. every scenario in this app's features RUNS, against a real `comp-host`
//!      serving the composed binder.
//!
//! The second is what stops a feature file becoming documentation. A scenario
//! nobody implemented fails loudly here; one that stops being true fails the build.
//!
//! ## The step vocabulary is deliberately small
//!
//! Eight steps, all of them about HTTP, because that is the app's whole surface.
//! A DSL that grows a step per assertion is a second test framework with worse
//! tooling — the moment a scenario needs something this cannot say, the honest
//! answer is a Rust test, and `binder.rs` is where those live.

mod harness;
use harness::{start_host_on, Client, PORTS};

use gherkin_validate::{parse, Severity};
use serde_json::Value;

/// Every `.feature` in the repository, not just this app's.
fn feature_files() -> Vec<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf();
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            // Not `target`, and not the corpus — those are cucumber's own test
            // data and half of them are deliberately broken.
            if p.is_dir() && !matches!(name.as_str(), "target" | ".git" | "node_modules" | "corpus")
            {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "feature") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Claim one: they are all valid.
///
/// Warnings are printed rather than failed — an empty scenario is a placeholder
/// somebody is mid-way through writing, and cucumber parses it. An ERROR means the
/// file is not Gherkin at all.
#[test]
fn every_feature_file_in_the_repository_is_valid_gherkin() {
    let files = feature_files();
    assert!(!files.is_empty(), "no .feature files found — the walk is wrong");
    let mut broken = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).expect("readable");
        for p in gherkin_validate::validate(&src) {
            let line = format!("{}:{}:{} {:?}", f.display(), p.line, p.column, p.kind);
            match p.severity() {
                Severity::Error => broken.push(line),
                _ => eprintln!("  warning: {line}"),
            }
        }
    }
    assert!(broken.is_empty(), "{} feature file(s) are broken:\n{}", broken.len(), broken.join("\n"));
    eprintln!("{} feature file(s) validated", files.len());
}

/// The fixtures a `When I upload` step can name. Small and explicit: a step that
/// can read any path is a test that can be made to pass by writing a file.
fn fixture(name: &str) -> Vec<u8> {
    match name {
        "cards.xlsx" => include_bytes!("cards.xlsx").to_vec(),
        "more.csv" => b"name,set_code,number,quantity,paid_minor,currency\n\
                        Gengar,fossil,5/62,1,32000,EUR\n\
                        \"Mr. Mime, holo\",jungle,6/64,2,15000,EUR\n"
            .to_vec(),
        "bad.csv" => b"name,quantity\nPidgey,2\nRattata,not-a-number\n".to_vec(),
        other => panic!("no fixture named {other:?} — add it to `fixture()`"),
    }
}

/// `"a.b.0.c"` into a JSON value. Enough for a response body, and no more.
fn field<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for part in path.split('.') {
        cur = match part.parse::<usize>() {
            Ok(i) => cur.get(i)?,
            Err(_) => cur.get(part)?,
        };
    }
    Some(cur)
}

/// The text between the next pair of quotes, from `at`.
fn quoted(s: &str, at: usize) -> Option<(String, usize)> {
    let open = s[at..].find('"')? + at;
    let close = s[open + 1..].find('"')? + open + 1;
    Some((s[open + 1..close].to_string(), close + 1))
}

struct World {
    client: Client,
    status: u16,
    body: Value,
    /// Values a scenario pulled out of one response to use in a later URL. A swap
    /// id is minted by the app, so no scenario can write it down in advance.
    remembered: std::collections::BTreeMap<String, String>,
}

impl World {
    /// `{name}` in a path becomes what an earlier step remembered.
    fn fill(&self, path: &str) -> String {
        let mut out = path.to_string();
        for (k, v) in &self.remembered {
            out = out.replace(&format!("{{{k}}}"), v);
        }
        out
    }
}

/// One step. Returns `Err` with a sentence when the step is not one we know, so an
/// unimplemented scenario fails as a MISSING STEP rather than as a wrong answer.
fn run_step(w: &mut World, text: &str) -> Result<(), String> {
    let t = text.trim();

    if let Some(rest) = t.strip_prefix("a signed-in collector ") {
        let (email, _) = quoted(rest, 0).ok_or("expected a quoted email")?;
        w.client.sign_in(&email);
        return Ok(());
    }
    if let Some(rest) = t.strip_prefix("I remember the field ") {
        let (path, after) = quoted(rest, 0).ok_or("expected a quoted field")?;
        let (name, _) = quoted(rest, after).ok_or("expected a quoted name")?;
        let v = field(&w.body, &path).ok_or_else(|| format!("no field `{path}` in {}", w.body))?;
        let as_text =
            v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string().trim_matches('"').to_string());
        w.remembered.insert(name, as_text);
        return Ok(());
    }
    if let Some(rest) = t.strip_prefix("I GET ") {
        let (path, after) = quoted(rest, 0).ok_or("expected a quoted path")?;
        let anonymous = rest[after..].contains("as nobody");
        let path = w.fill(&path);
        let (s, b) = w.client.get(&path, !anonymous);
        w.status = s;
        w.body = b;
        return Ok(());
    }
    if let Some(rest) = t.strip_prefix("I upload ") {
        let (name, after) = quoted(rest, 0).ok_or("expected a quoted fixture name")?;
        let (path, _) = quoted(rest, after).ok_or("expected a quoted path")?;
        let path = w.fill(&path);
        let (s, b) = w.client.upload(&path, &fixture(&name));
        w.status = s;
        w.body = b;
        return Ok(());
    }
    if let Some(rest) = t.strip_prefix("I POST ") {
        let (path, _) = quoted(rest, 0).ok_or("expected a quoted path")?;
        let path = w.fill(&path);
        let (s, b) = w.client.post(&path, w.body.clone());
        w.status = s;
        w.body = b;
        return Ok(());
    }
    if let Some(rest) = t.strip_prefix("the response is ") {
        let want: u16 = rest.trim().parse().map_err(|_| "expected a status code")?;
        if w.status != want {
            return Err(format!("expected {want}, got {} — {}", w.status, w.body));
        }
        return Ok(());
    }
    if let Some(rest) = t.strip_prefix("the field ") {
        let (path, after) = quoted(rest, 0).ok_or("expected a quoted field")?;
        let tail = rest[after..].trim();
        let got = field(&w.body, &path)
            .ok_or_else(|| format!("no field `{path}` in {}", w.body))?;

        if let Some(n) = tail.strip_prefix("has ").and_then(|x| x.split(' ').next()) {
            let want: usize = n.parse().map_err(|_| "expected a count")?;
            let len = got.as_array().map(|a| a.len()).ok_or("not a list")?;
            return if len == want {
                Ok(())
            } else {
                Err(format!("expected {want} entries in `{path}`, got {len}"))
            };
        }
        let want = tail.strip_prefix("is ").ok_or("expected `is` or `has N entries`")?.trim();
        let want: Value = serde_json::from_str(want)
            .unwrap_or_else(|_| Value::String(want.trim_matches('"').to_string()));
        return if *got == want {
            Ok(())
        } else {
            Err(format!("`{path}`: expected {want}, got {got}"))
        };
    }
    if let Some(rest) = t.strip_prefix("the body contains ") {
        let (needle, _) = quoted(rest, 0).ok_or("expected quoted text")?;
        return if w.body.to_string().contains(&needle) {
            Ok(())
        } else {
            Err(format!("`{needle}` is not in the body"))
        };
    }

    Err(format!("no step matches `{t}` — see the vocabulary in features.rs"))
}

/// Claim two: every scenario in this app's features runs.
#[test]
fn every_scenario_runs_against_the_composed_app() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("features");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("features/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "feature"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no features to run");

    let mut ran = 0usize;
    for path in files {
        let src = std::fs::read_to_string(&path).expect("readable");
        let doc = parse(&src)
            .unwrap_or_else(|ps| panic!("{} is not valid Gherkin: {ps:?}", path.display()));

        for scenario in &doc.scenarios {
            // An outline is one run per Examples row, with `<placeholder>`
            // substituted. A plain scenario is one run with nothing to substitute.
            let cases: Vec<Vec<(String, String)>> = if scenario.examples.is_empty() {
                vec![Vec::new()]
            } else {
                scenario
                    .examples
                    .iter()
                    .flat_map(|t| {
                        t.rows.iter().map(|r| {
                            t.header.iter().cloned().zip(r.iter().cloned()).collect::<Vec<_>>()
                        })
                    })
                    .collect()
            };

            for case in cases {
                // A host per scenario, so one scenario's collection can never be
                // another's starting state — the bug that made two tests in
                // `binder.rs` share a port and a store.
                let _host = start_host_on(PORTS[2]);
                let mut w = World {
                    client: Client::new(),
                    status: 0,
                    body: Value::Null,
                    remembered: std::collections::BTreeMap::new(),
                };
                let steps = doc.background.iter().chain(scenario.steps.iter());
                for step in steps {
                    let mut text = step.text.clone();
                    for (k, v) in &case {
                        text = text.replace(&format!("<{k}>"), v);
                    }
                    // A docstring is the body of the request the next step makes.
                    if !step.argument.is_empty() {
                        let joined = step.argument.join("\n");
                        if let Ok(v) = serde_json::from_str::<Value>(&joined) {
                            w.body = v;
                        }
                    }
                    if let Err(why) = run_step(&mut w, &text) {
                        panic!(
                            "{}\n  scenario: {}\n  step: {} {}\n  {}",
                            path.display(),
                            scenario.name,
                            step.keyword,
                            text,
                            why
                        );
                    }
                }
                ran += 1;
            }
        }
    }
    eprintln!("{ran} scenario run(s) executed");
    assert!(ran >= 8, "expected the features to produce at least 8 runs, got {ran}");
}
