//! `tool_build`: an agent writes a tool in MoonBit, and it is compiled from a template
//! (`components/text-stats-mbt`), started on a lattice node, written as a `wrpc` connector and
//! granted to that agent, all in about a second. The tool is a component exporting `holon:tool/tool`;
//! its only import is `wasi:http/types`, which the template's world fixes, so new code reaches no more
//! than any tenant component does.
//!
//! Set up by environment: `HOLON_FORGE_TEMPLATE` (the template directory) and
//! `HOLON_FORGE_ARTIFACTS` (the node's `<state-dir>/artifacts`) are required; `HOLON_FORGE_LATTICE`
//! (`default`), `HOLON_FORGE_NODE` (this host's name), `HOLON_NATS_URL`, `MOON` and `WASM_TOOLS`
//! are optional.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::connector::{Definition, Exec, Wrpc};

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Builds `args.source` as tool `args.name` for `agent`, starts it and writes its connector.
/// Granting it is the caller's: that is a change to the agent, not to the lattice.
pub fn build(
    work_root: &Path,
    connectors: &Path,
    agent: &str,
    args: &Value,
) -> Result<String, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let (name, source) = (s("name"), s("source"));
    if !valid_name(&name) || crate::agent::tool_def(&name).is_some() {
        return Err(format!("`{name}` cannot be a tool name: use lowercase letters, digits and _, starting with a letter, and not a built-in tool's name"));
    }
    let template = env("HOLON_FORGE_TEMPLATE")
        .ok_or("building tools is not set up here (HOLON_FORGE_TEMPLATE is unset)")?;
    let artifacts = env("HOLON_FORGE_ARTIFACTS")
        .ok_or("building tools is not set up here (HOLON_FORGE_ARTIFACTS is unset)")?;
    let lattice = env("HOLON_FORGE_LATTICE").unwrap_or_else(|| "default".into());
    let node = env("HOLON_FORGE_NODE").or_else(hostname).ok_or("no HOLON_FORGE_NODE")?;
    let wrpc = Wrpc { lattice: lattice.clone(), instance: format!("agents/{agent}/{name}") };

    // A connector is global by name: never take over one this agent did not build.
    if let Some(d) = crate::connector::load(connectors, &name) {
        if d.exec.wrpc.as_ref() != Some(&wrpc) {
            return Err(format!("a tool named `{name}` already exists; choose another name"));
        }
    }

    let t0 = Instant::now();
    let work = work_root.join(format!("{agent}-{name}"));
    let _ = std::fs::remove_dir_all(&work);
    copy_tree(Path::new(&template), &work).map_err(|e| format!("copying the template: {e}"))?;
    customize_template(&work, &name)?;
    std::fs::write(work.join("tool/tool.mbt"), source).map_err(|e| e.to_string())?;
    let wasm = compile(&work)?;
    let build_ms = t0.elapsed().as_millis();

    let digest = format!("{:x}", Sha256::digest(&wasm));
    let artifacts = PathBuf::from(artifacts);
    std::fs::create_dir_all(&artifacts).map_err(|e| e.to_string())?;
    let tmp = artifacts.join(format!(".{digest}.tmp"));
    std::fs::write(&tmp, &wasm).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, artifacts.join(format!("{digest}.wasm"))).map_err(|e| e.to_string())?;

    start(&lattice, &node, agent, &name, &digest)?;

    let def = Definition {
        description: s("description"),
        exec: Exec {
            command: Vec::new(),
            url: None,
            wrpc: Some(wrpc),
            args: if s("args").is_empty() { "{}".into() } else { s("args") },
            required: args
                .get("required")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default(),
            sensitive: false,
            timeout_secs: 10,
        },
    };
    std::fs::create_dir_all(connectors).map_err(|e| e.to_string())?;
    std::fs::write(
        connectors.join(format!("{name}.json")),
        serde_json::to_string_pretty(&def).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    Ok(format!(
            "built `{name}` in {build_ms} ms ({} KB) and started it; it is yours now. Call it as {{\"tool\": \"{name}\", \"args\": {}}}",
            wasm.len() / 1024,
            def.exec.args
    ))
}

fn hostname() -> Option<String> {
    let out = Command::new("hostname").arg("-s").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|h| !h.is_empty())
}

/// The template without its build output, following its `wit/deps` links so the copy stands alone.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_str(), Some("_build" | ".mooncakes" | "target" | "text")) {
            continue;
        }
        let path = entry.path();
        if std::fs::metadata(&path)?.is_dir() {
            copy_tree(&path, &to.join(&name))?;
        } else {
            std::fs::copy(&path, to.join(&name))?;
        }
    }
    Ok(())
}

pub(crate) fn customize_template(work: &Path, name: &str) -> Result<(), String> {
    let slug = name.replace('_', "-");
    let mod_path = work.join("moon.mod");
    if let Ok(content) = std::fs::read_to_string(&mod_path) {
        let updated = content.replace("holon/text-stats", &format!("holon/{slug}"));
        std::fs::write(&mod_path, updated).map_err(|e| format!("moon.mod: {e}"))?;
    }
    let wit_path = work.join("wit/world.wit");
    if let Ok(content) = std::fs::read_to_string(&wit_path) {
        let updated = content
            .replace("package holon:text-stats@0.1.0;", &format!("package holon:{slug}@0.1.0;"))
            .replace("world text-stats {", &format!("world {slug} {{"));
        std::fs::write(&wit_path, updated).map_err(|e| format!("wit/world.wit: {e}"))?;
    }
    let old_world = work.join("world/text-stats");
    let new_world = work.join(format!("world/{slug}"));
    if old_world.exists() && old_world != new_world {
        std::fs::rename(&old_world, &new_world).map_err(|e| format!("rename world dir: {e}"))?;
    }
    rewrite_moon_pkgs(work, &slug).map_err(|e| format!("moon.pkg rewrites: {e}"))?;
    let _ = std::fs::remove_dir_all(work.join("text"));
    let _ = std::fs::remove_file(work.join("tool.mbt"));
    let readme = format!(
        "# {slug} (MoonBit)\n\n\
        An agent tool as a component: `holon:tool/tool.call` (`wit/tool/tool.wit`) for an agent-runtime\n\
        connector's `wrpc`, and `wasi:http/handler` (p3) for its `url`. Both take a JSON object of arguments\n\
        and answer JSON, or an error the model reads.\n\n\
        ```bash\n\
        tools/build-polyglot.sh mbt {slug}\n\
        ```\n"
    );
    let _ = std::fs::write(work.join("README.md"), readme);
    Ok(())
}

fn rewrite_moon_pkgs(dir: &Path, slug: &str) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !matches!(name, "_build" | "target" | ".mooncakes") {
                rewrite_moon_pkgs(&path, slug)?;
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some("moon.pkg") {
            let content = std::fs::read_to_string(&path)?;
            if content.contains("holon/text-stats") {
                let updated = content.replace("holon/text-stats", &format!("holon/{slug}"));
                std::fs::write(&path, updated)?;
            }
        }
    }
    Ok(())
}

/// `moon build`, then the component, embedded UTF-16 (MoonBit's strings). A compile error is
/// returned as the compiler wrote it, for the agent to fix.
fn compile(work: &Path) -> Result<Vec<u8>, String> {
    let moon = env("MOON").unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.moon/bin/moon")
    });
    let out = Command::new(&moon)
        .args(["build", "--target", "wasm", "--release"])
        .current_dir(work)
        .output()
        .map_err(|e| format!("{moon}: {e}"))?;
    if !out.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .replace(&format!("{}/", work.canonicalize().unwrap_or(work.into()).display()), "");
        return Err(format!("tool.mbt does not compile:\n{}", compiler_errors(&text)));
    }
    let world = std::fs::read_to_string(work.join("wit/world.wit"))
        .ok()
        .and_then(|w| {
            w.lines().find_map(|l| {
                l.trim().strip_prefix("world ").map(|r| r.trim_end_matches(['{', ' ']).to_string())
            })
        })
        .ok_or("the template's wit/world.wit names no world")?;
    let tools = env("WASM_TOOLS").unwrap_or_else(|| "wasm-tools".into());
    let (core, embedded, component) = (
        work.join("_build/wasm/release/build/gen/gen.wasm"),
        work.join("embedded.wasm"),
        work.join("tool.wasm"),
    );
    let run = |args: &[&str]| -> Result<(), String> {
        let o = Command::new(&tools)
            .args(args)
            .current_dir(work)
            .output()
            .map_err(|e| format!("{tools}: {e}"))?;
        o.status
            .success()
            .then_some(())
            .ok_or_else(|| String::from_utf8_lossy(&o.stderr).trim().to_string())
    };
    let p = |p: &Path| p.to_string_lossy().into_owned();
    run(&[
        "component",
        "embed",
        "wit",
        "--world",
        &world,
        "--encoding",
        "utf16",
        &p(&core),
        "-o",
        &p(&embedded),
    ])?;
    run(&["component", "new", &p(&embedded), "-o", &p(&component)])?;
    let bytes = std::fs::read(&component).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&embedded);
    let _ = std::fs::remove_file(&component);
    Ok(bytes)
}

/// The compiler's errors, without its warnings about the generated bindings.
fn compiler_errors(text: &str) -> String {
    let mut out = String::new();
    let mut keep = false;
    for line in text.lines() {
        if line.starts_with("Error") {
            keep = true;
        } else if line.starts_with("Warning") || line.starts_with("Failed with") {
            keep = false;
        }
        if keep && out.len() < 4000 {
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        text.chars().rev().take(2000).collect::<String>().chars().rev().collect()
    } else {
        out
    }
}

/// Asks `node` to start the artifact as `agents/<agent>/<name>`; acked once it serves.
fn start(lattice: &str, node: &str, agent: &str, name: &str, digest: &str) -> Result<(), String> {
    let nats_url =
        std::env::var("HOLON_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let cmd = json!({"tenant": "agents", "app": agent, "component": name, "digest": format!("sha256:{digest}")});
    let subject = holon_lattice::wire::command_subject(lattice, node, "start");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let reply = rt.block_on(async {
        let nats = async_nats::connect(&nats_url).await.map_err(|e| format!("{nats_url}: {e}"))?;
        let req = nats.request(subject, cmd.to_string().into());
        match tokio::time::timeout(Duration::from_secs(60), req).await {
            Err(_) => Err(format!("node {node} did not start the tool within 60s")),
            Ok(Err(e)) => Err(format!("node {node} on lattice {lattice}: {e}")),
            Ok(Ok(m)) => Ok(m.payload),
        }
    })?;
    let v: Value = serde_json::from_slice(&reply).unwrap_or(Value::Null);
    match v.get("error").and_then(Value::as_str) {
        Some(e) => Err(format!("node {node} could not start it: {e}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_plain_and_never_a_built_in() {
        assert!(valid_name("word_freq") && valid_name("t2"));
        assert!(!valid_name("") && !valid_name("2t") && !valid_name("Word") && !valid_name("a-b"));
        assert!(!valid_name("../x") && !valid_name(&"a".repeat(41)));
        let err = build(
            Path::new("/nonexistent"),
            Path::new("/nonexistent"),
            "a",
            &json!({"name": "http_get", "source": ""}),
        );
        assert!(err.err().unwrap().contains("cannot be a tool name"));
    }

    #[test]
    fn only_the_compiler_errors_come_back() {
        let out = "Warning: [0020]\n  noise\nError: [4015]\n  x.mbt:3 has no method\nWarning: y\nFailed with 1 errors.\n";
        assert_eq!(compiler_errors(out), "Error: [4015]\n  x.mbt:3 has no method\n");
    }

    #[test]
    fn another_agents_tool_is_not_taken_over() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("calc.json"), r#"{"command":["bc"]}"#).unwrap();
        std::env::set_var("HOLON_FORGE_TEMPLATE", "/t");
        std::env::set_var("HOLON_FORGE_ARTIFACTS", "/a");
        std::env::set_var("HOLON_FORGE_NODE", "n1");
        let err = build(d.path(), d.path(), "a", &json!({"name": "calc", "source": ""}));
        assert!(err.err().unwrap().contains("already exists"));
    }

    #[test]
    fn template_customization_renames_project_and_prunes_leftovers() {
        let d = tempfile::tempdir().unwrap();
        let work = d.path();
        std::fs::write(
            work.join("moon.mod"),
            "name = \"holon/text-stats\"\npreferred_target = \"wasm\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(work.join("wit")).unwrap();
        std::fs::write(
            work.join("wit/world.wit"),
            "package holon:text-stats@0.1.0;\n\nworld text-stats {\n    export holon:tool/tool@0.1.0;\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(work.join("world/text-stats")).unwrap();
        std::fs::write(work.join("world/text-stats/moon.pkg"), "warnings = \"-44\"\n").unwrap();
        std::fs::create_dir_all(work.join("gen")).unwrap();
        std::fs::write(work.join("gen/moon.pkg"), "import {\n  \"holon/text-stats/tool\",\n}\n")
            .unwrap();
        std::fs::create_dir_all(work.join("text")).unwrap();
        std::fs::write(work.join("text/text.mbt"), "old text").unwrap();
        std::fs::write(work.join("tool.mbt"), "stray root tool").unwrap();

        customize_template(work, "prime_count").unwrap();

        let moon_mod = std::fs::read_to_string(work.join("moon.mod")).unwrap();
        assert!(moon_mod.contains("name = \"holon/prime-count\""));
        assert!(!moon_mod.contains("text-stats"));

        let world_wit = std::fs::read_to_string(work.join("wit/world.wit")).unwrap();
        assert!(world_wit.contains("package holon:prime-count@0.1.0;"));
        assert!(world_wit.contains("world prime-count {"));

        assert!(work.join("world/prime-count/moon.pkg").exists());
        assert!(!work.join("world/text-stats").exists());

        let gen_pkg = std::fs::read_to_string(work.join("gen/moon.pkg")).unwrap();
        assert!(gen_pkg.contains("holon/prime-count/tool"));
        assert!(!gen_pkg.contains("text-stats"));

        assert!(!work.join("text").exists(), "text/ leftover is removed");
        assert!(!work.join("tool.mbt").exists(), "stray root tool.mbt is removed");

        let readme = std::fs::read_to_string(work.join("README.md")).unwrap();
        assert!(readme.starts_with("# prime-count (MoonBit)"));
    }
}
