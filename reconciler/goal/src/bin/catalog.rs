//! The component catalogue from sources (what a component is for); `comp-capgraph` reads artifacts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use comp_fleet::fleet::repo_root;
use regex::Regex;
use serde::Serialize;

#[derive(Serialize)]
struct ConfigKey {
    name: String,
    description: String,
}

#[derive(Serialize)]
struct Entry {
    name: String,
    package: String,
    description: String,
    exports: Vec<String>,
    capability_deps: Vec<String>,
    config_keys: Vec<ConfigKey>,
    reusable_as_is: bool,
    /// Detected from the source, so implementing the thing clears the flag.
    unimplemented: bool,
}

/// `target.path` may point at a shared dir (`auth-guard` uses the root `wit/`).
fn component_wit_dir(dir: &Path) -> Option<PathBuf> {
    let manifest = dir.join("Cargo.toml");
    if let Ok(text) = std::fs::read_to_string(&manifest) {
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

/// A shared wit directory holds many worlds; reading all of them misattributed exports.
fn target_world(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let re =
        Regex::new(r#"(?ms)\[package\.metadata\.component\.target\][^\[]*?^world\s*=\s*"([^"]+)""#)
            .unwrap();
    re.captures(&text).map(|c| c[1].to_string())
}

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
    // Comments stripped first: an unanchored `export` pattern matched prose.
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
    // The whole text when it names none (a single-world local `wit/`).
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
            for block in res.config_block.captures_iter(&text) {
                for line in res.config_line.captures_iter(&block[1]) {
                    let name = line[1].to_string();
                    if seen.insert(name.clone()) {
                        config_keys
                            .push(ConfigKey { name, description: line[2].trim().to_string() });
                    }
                }
            }
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
    let reusable_as_is = comp_goal::catalogue::offers_a_contract(&exports);
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
            get_call: Regex::new(r#"[a-z_]*(?:get|cfg)[a-z_0-9]*\(\s*"([a-z0-9._-]{2,})""#)
                .unwrap(),
            config_block: Regex::new(r"(?m)^//! Config[^\n]*:\n((?://!.*\n)+)").unwrap(),
            config_line: Regex::new(r"(?m)^//!\s{2,}([a-z0-9._-]{2,})\s{2,}(.+?)\s*$").unwrap(),
        }
    }
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

    println!("{}", serde_json::to_string_pretty(&entries)?);
    Ok(())
}
