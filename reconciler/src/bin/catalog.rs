//! `comp-catalog` — the component catalogue, from the components' own sources.
//!
//! What a consumer needs in order to adopt a component WITHOUT reading its source:
//! the WIT package and exports, the dependency footprint, the `wasi:config` knobs it
//! reads, and the one-line description from its module doc. Written as
//! Printed as JSON with `--json`, and rendered into `components/CATALOG.md` for
//! people. Deliberately NOT a committed `catalog.json`: see the note in `main`.
//!
//! ## Why this is not `comp-capgraph`
//!
//! They answer different questions from different evidence, and the difference is
//! load-bearing. `comp-capgraph` reads the BUILT ARTIFACTS: what a component really
//! imports, after the linker has dropped everything unreachable. This reads the
//! SOURCE: what a component says it is for. A component that does not build has no
//! artifact and vanishes from the graph; it still has a description here.
//!
//! ## Why this is not Python any more
//!
//! It was `tools/gen-catalog.py`, and the port is not about taste. The catalogue is
//! read by `capsearch`, which is what stops a goal from generating a capability the
//! pool already has (ADR-0089) — so it is load-bearing, and it had no test, because
//! it could not have one: it embedded `wasm_size_bytes` and `wasm_sha256_12` from
//! the last build and was therefore stale by construction. With those gone it is a
//! function of the source, and a function of the source can be checked. Being in the
//! same workspace as `reconciler/tests/derived.rs` is what makes that cheap.
//!
//! One deliberate difference from the Python: non-ASCII is written as itself. Python
//! escapes it by default (`ensure_ascii=True`), which turned 256 em-dashes into
//! `—`. Both are valid JSON and one of them is readable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use comp_reconciler::fleet::repo_root;
use regex::Regex;
use serde::Serialize;

/// Probes the `-probe` suffix does not catch. Reusable-shaped, but they exist to test
/// the thing they probe, so they are grouped away from the shopping list. Presentation
/// only: `reusable_as_is` is decided by the exports, never by this.
const PROBES: &[&str] = &["adversary", "twofile", "bigadd", "demo", "mock-fitness", "mock-provider"];

#[derive(Serialize)]
struct ConfigKey {
    name: String,
    description: String,
}

/// Field order is the SERIALISED order, and it is the committed file's order.
#[derive(Serialize)]
struct Entry {
    name: String,
    package: String,
    description: String,
    exports: Vec<String>,
    capability_deps: Vec<String>,
    config_keys: Vec<ConfigKey>,
    reusable_as_is: bool,
    /// A component whose exports all return an `UNIMPLEMENTED:` marker is a CONTRACT,
    /// not a capability. Detected from the source rather than kept in a list, because
    /// a list is a second place to forget: implement the thing and the flag clears
    /// itself.
    unimplemented: bool,
}

/// Where this crate's WIT actually lives.
///
/// A local `wit/` is the common case and was once assumed to be the only one. It is
/// not: a crate may point `[package.metadata.component.target].path` at a shared
/// directory, and `auth-guard` does — it uses the repo-root `wit/` so it can share the
/// wkg-vendored `wasi:*` packages. That assumption made the single most-depended-on
/// capability in the tree invisible to `capsearch`: 36 applications import
/// `auth:guard`, and a goal asking to let users log in could not find it, because a
/// crate with no local `wit/` was skipped before its description was ever read.
fn component_wit_dir(dir: &Path) -> Option<PathBuf> {
    let manifest = dir.join("Cargo.toml");
    if let Ok(text) = std::fs::read_to_string(&manifest) {
        // Only trust the path when the crate declares itself a component at all.
        if text.contains("[package.metadata.component]") {
            let re = Regex::new(
                r#"(?ms)\[package\.metadata\.component\.target\][^\[]*?^path\s*=\s*"([^"]+)""#,
            )
            .unwrap();
            if let Some(c) = re.captures(&text) {
                let candidate = dir.join(&c[1]);
                if candidate.is_dir() {
                    return std::fs::canonicalize(candidate).ok();
                }
            }
        }
    }
    let local = dir.join("wit");
    local.is_dir().then_some(local)
}

/// The first non-empty `//!` line, trailing full stop removed.
fn first_doc_line(lib: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(lib) else { return String::new() };
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("//!") {
            let t = rest.trim();
            if !t.is_empty() {
                return t.trim_end_matches('.').to_string();
            }
        } else if !line.is_empty() && !line.starts_with("//") {
            break;
        }
    }
    String::new()
}


/// The world this crate builds, from `[package.metadata.component.target].world`.
///
/// Needed because a crate may point at a SHARED wit directory — `accounts-app` and
/// `auth-guard` use the repo-root `wit/` so they can share the wkg-vendored `wasi:*`
/// packages — and that directory holds many worlds. Reading exports from all of them
/// attributed an unrelated `types` interface to `accounts-app` and `sample-consumer`,
/// and told the catalogue they offered a contract when the built artifact exports
/// nothing but a door.
fn target_world(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let re = Regex::new(r#"(?ms)\[package\.metadata\.component\.target\][^\[]*?^world\s*=\s*"([^"]+)""#)
        .unwrap();
    re.captures(&text).map(|c| c[1].to_string())
}

/// The body of `world <name> { ... }`, brace-matched.
fn world_body<'a>(text: &'a str, world: &str) -> Option<&'a str> {
    let at = text.find(&format!("world {world} "))?;
    let open = at + text[at..].find('{')?;
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn scan(dir: &Path, res: &Regexes) -> Option<Entry> {
    let wit_dir = component_wit_dir(dir)?;
    let mut wits: Vec<PathBuf> = std::fs::read_dir(&wit_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wit"))
        .collect();
    wits.sort();
    let lib = dir.join("src/lib.rs");
    if wits.is_empty() || !lib.is_file() {
        return None;
    }
    // Comments stripped FIRST, and that is not fussiness. A world may be written on
    // one line — `world calc { export arith; }` — so the export pattern cannot be
    // anchored to the start of a line, and an unanchored one matches the word
    // "export" inside prose. It captured `"and a PII-redacted audit view — every
    // cross-cutting concern…"` as an interface name and reclassified seven
    // components on the strength of it.
    let wit_text = wits
        .iter()
        .filter_map(|w| std::fs::read_to_string(w).ok())
        .map(|t| {
            t.lines()
                .map(|l| match l.find("//") {
                    Some(at) => &l[..at],
                    None => l,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let package =
        res.package.captures(&wit_text).map(|c| c[1].trim().to_string()).unwrap_or_default();
    // Only the world this crate actually builds. Falls back to the whole text when the
    // crate names no world, which is the common case of a component with its own
    // single-world `wit/`.
    let scope = target_world(dir)
        .and_then(|w| world_body(&wit_text, &w).map(str::to_string))
        .unwrap_or_else(|| wit_text.clone());
    let exports: Vec<String> =
        res.export.captures_iter(&scope).map(|c| c[1].trim().to_string()).collect();
    let deps: Vec<String> = res
        .import
        .captures_iter(&scope)
        .map(|c| c[1].trim().split('@').next().unwrap_or_default().to_string())
        .filter(|i| !res.std_wasi.is_match(i))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut config_keys = Vec::new();
    if deps.iter().any(|d| d.starts_with("wasi:config")) {
        let mut seen = BTreeSet::new();
        let mut sources: Vec<PathBuf> = std::fs::read_dir(dir.join("src"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|e| e == "rs")
                    && p.file_name().is_some_and(|n| n != "bindings.rs")
            })
            .collect();
        sources.sort();
        for src in sources {
            let Ok(text) = std::fs::read_to_string(&src) else { continue };
            // Preferred: the documented Config block, which carries a description.
            for block in res.config_block.captures_iter(&text) {
                for line in res.config_line.captures_iter(&block[1]) {
                    let name = line[1].to_string();
                    if seen.insert(name.clone()) {
                        config_keys
                            .push(ConfigKey { name, description: line[2].trim().to_string() });
                    }
                }
            }
            // Fallback: string literals at get/cfg call sites, undocumented.
            for c in res.get_call.captures_iter(&text) {
                let name = c[1].to_string();
                if seen.insert(name.clone()) {
                    config_keys.push(ConfigKey { name, description: String::new() });
                }
            }
        }
    }

    let name = dir.file_name()?.to_string_lossy().to_string();
    let unimplemented =
        std::fs::read_to_string(&lib).map(|t| t.contains("UNIMPLEMENTED:")).unwrap_or(false);
    let reusable_as_is = comp_reconciler::catalogue::offers_a_contract(&exports);
    Some(Entry {
        reusable_as_is,
        description: first_doc_line(&lib),
        name,
        package,
        exports,
        capability_deps: deps,
        config_keys,
        unimplemented,
    })
}

struct Regexes {
    package: Regex,
    export: Regex,
    import: Regex,
    std_wasi: Regex,
    get_call: Regex,
    config_block: Regex,
    config_line: Regex,
}

impl Regexes {
    fn new() -> Self {
        Self {
            package: Regex::new(r"(?m)^package\s+([^;]+);").unwrap(),
            export: Regex::new(r"export\s+([^;{]+);").unwrap(),
            import: Regex::new(r"import\s+([^;{]+);").unwrap(),
            std_wasi: Regex::new(r"^wasi:(clocks|random|io|cli|filesystem|sockets)/").unwrap(),
            get_call: Regex::new(r#"[a-z_]*(?:get|cfg)[a-z_0-9]*\(\s*"([a-z0-9._-]{2,})""#).unwrap(),
            // The repo convention: a module-doc block listing knobs with descriptions.
            config_block: Regex::new(r"(?m)^//! Config[^\n]*:\n((?://!.*\n)+)").unwrap(),
            config_line: Regex::new(r"(?m)^//!\s{2,}([a-z0-9._-]{2,})\s{2,}(.+?)\s*$").unwrap(),
        }
    }
}

/// GitHub heading anchor: lowercase, drop punctuation, spaces to dashes.
fn slugify(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
        .collect::<String>()
        .replace(' ', "-")
}

fn dep_badge(deps: &[String]) -> String {
    if deps.is_empty() {
        return "pure compute".to_string();
    }
    deps.iter()
        .map(|d| d.replace("wasi:keyvalue/", "kv:").replace("wasi:", ""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn role(e: &Entry) -> &'static str {
    if e.unimplemented {
        return "contract";
    }
    // Before the plugability question, because a probe that exports only a door would
    // otherwise be filed as an application, and it is not one.
    if e.name.ends_with("-probe") || PROBES.contains(&e.name.as_str()) {
        return "probe";
    }
    if !e.reusable_as_is {
        return "app";
    }
    "capability"
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = repo_root();
    let components = root.join("components");
    let res = Regexes::new();

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&components)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    dirs.sort();

    let entries: Vec<Entry> = dirs.iter().filter_map(|d| scan(d, &res)).collect();

    // `--json` prints; nothing writes a `catalog.json`.
    //
    // There was one, committed, "for tooling" — and by the time `capsearch` stopped
    // reading it, the only things left reading it were the tests checking whether it
    // had gone stale. A file that exists to be verified rather than used is a
    // liability with a guard bolted on: the guard exists because the file does.
    //
    // The component IS the source of truth. This reads them and answers; a caller
    // that wants the answer runs it. That costs about a second and cannot drift.
    if std::env::args().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    let groups: [(&str, &str, &str); 4] = [
        ("capability", "Capabilities — reusable as-is",
         "Each one exports a contract outside the `wasi:` namespace, so it drops into another app via `wac plug` or a link, configured through `wasi:config`. That export IS the membership test — this is the part of the tree meant to be reused, and [ADR-0089](../docs/adr/0089-capability-accumulation.md) is why a gate makes you look here before you build."),
        ("app", "Nothing to plug — exports only a door",
         "These export `wasi:http/incoming-handler` and nothing else, so there is no contract for `wac plug` or a link to satisfy: they are consumers of the capabilities above rather than one of them. Membership is read off the exports, not off the name — a `-domain` suffix and a list of ten exceptions disagreed with the components themselves 33 times, in both directions, advertising every probe and all five `eshop-*` parts as reusable while hiding `login-app`'s `login:app/auth` from search. Most are whole applications; one file each in [`docs/apps/`](../docs/apps/README.md)."),
        ("contract", "Contract only — no implementation behind the WIT",
         "The WIT is real and every export returns an `UNIMPLEMENTED:` marker. These need a host-side capability a wasm guest cannot have (a syscall, a socket, a subprocess), so they state what a host must satisfy rather than satisfying it — see [ADR-0095](../docs/adr/0095-what-is-allowed-to-be-native.md)."),
        ("probe", "Probes — test harnesses",
         "Built to exercise one capability from the outside and prove it is linkable. Reusable, but only useful if you are testing the thing they probe."),
    ];

    let mut buckets: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for e in &entries {
        buckets.entry(role(e)).or_default().push(e);
    }
    let empty: Vec<&Entry> = Vec::new();
    let of = |k: &str| buckets.get(k).unwrap_or(&empty);

    let n = entries.len();
    let mut lines: Vec<String> = vec![
        "# Component catalog".into(),
        String::new(),
        format!("Generated by `comp-catalog` — do not edit by hand. {n} components."),
        String::new(),
        format!("Grouped by what a component *is*, because one alphabetical table of {n} rows answered no question anybody actually asks. For what a component really imports (read out of the built wasm rather than its source), see [`docs/CAPABILITY-GRAPH.md`](../docs/CAPABILITY-GRAPH.md)."),
        String::new(),
    ];
    for (key, title, _) in &groups {
        lines.push(format!("- [{title}](#{}) — {}", slugify(title), of(key).len()));
    }
    lines.push("- [Descriptions](#descriptions) — all of them, alphabetical".into());
    lines.push(String::new());

    for (key, title, blurb) in &groups {
        lines.extend([
            format!("## {title}"),
            String::new(),
            blurb.to_string(),
            String::new(),
            "| component | package | deps | config knobs |".into(),
            "|---|---|---|---|".into(),
        ]);
        for e in of(key) {
            let knobs = if e.config_keys.is_empty() {
                "—".to_string()
            } else {
                e.config_keys.iter().map(|k| format!("`{}`", k.name)).collect::<Vec<_>>().join(", ")
            };
            lines.push(format!(
                "| **{}** | `{}` | {} | {} |",
                e.name,
                e.package,
                dep_badge(&e.capability_deps),
                knobs
            ));
        }
        lines.push(String::new());
    }

    lines.extend(["## Descriptions".to_string(), String::new()]);
    for e in &entries {
        if !e.description.is_empty() {
            lines.push(format!("- **{}** — {}", e.name, e.description));
        }
    }
    lines.push(String::new());
    std::fs::write(components.join("CATALOG.md"), lines.join("\n"))?;

    println!("{n} components -> components/CATALOG.md");
    Ok(())
}
