//! `comp` talking to the platform.
//!
//! Everything here is one HTTP call and a bit of printing. It is deliberately thin:
//! the platform decides, and a CLI that re-implemented any of its rules would be a
//! second place for them to be wrong.
//!
//! The session token lives in `~/.config/comp/credentials.json`, mode 0600. Not a
//! keyring: this is a bearer token with an expiry, the same thing every `~/.netrc`
//! and `~/.docker/config.json` on the box already holds in the clear, and a keyring
//! dependency would buy nothing a file permission does not.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Serialize, Deserialize, Default)]
pub struct Session {
    pub url: String,
    pub token: String,
    pub tenant: String,
}

fn creds_path() -> PathBuf {
    std::env::var("COMP_CREDENTIALS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config/comp/credentials.json")
        })
}

pub fn load() -> Result<Session> {
    let p = creds_path();
    let raw = std::fs::read(&p)
        .with_context(|| format!("no session at {} — run `comp login` first", p.display()))?;
    Ok(serde_json::from_slice(&raw).context("credentials file is not readable JSON")?)
}

fn save(s: &Session) -> Result<()> {
    let p = creds_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&p, serde_json::to_vec_pretty(s)?)?;
    // The token is a credential. 0600 before anyone else on the box can read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// One request. Returns the parsed body, or an error carrying whatever the platform
/// said — its 422s name the component and the interface, and swallowing that in
/// favour of "request failed" would be the single most annoying thing this tool
/// could do.
fn call(
    s: &Session,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
    content_type: &str,
) -> Result<Value> {
    let url = format!("{}{}", s.url.trim_end_matches('/'), path);
    let mut req = ureq::request(method, &url);
    if !s.token.is_empty() {
        req = req.set("authorization", &format!("Bearer {}", s.token));
    }
    req = req.set("content-type", content_type);

    let res = match body {
        Some(b) => req.send_bytes(&b),
        None => req.call(),
    };
    match res {
        Ok(r) => {
            let mut buf = String::new();
            r.into_reader().read_to_string(&mut buf).ok();
            Ok(serde_json::from_str(&buf).unwrap_or(json!({ "raw": buf })))
        }
        Err(ureq::Error::Status(code, r)) => {
            let mut buf = String::new();
            r.into_reader().read_to_string(&mut buf).ok();
            let v: Value = serde_json::from_str(&buf).unwrap_or(json!({ "error": buf }));
            bail!("{code}: {}", explain(&v))
        }
        Err(e) => bail!("{url}: {e}"),
    }
}

/// Turn the platform's error body into something worth reading.
///
/// The structured 422 is the one that matters: an unsatisfied import is not a
/// sentence, it is a list of gaps each with the components that would fill it, and
/// printing it as JSON would waste the work the platform did to compute it.
fn explain(v: &Value) -> String {
    if v["error"] == json!("unsatisfied_imports") {
        let mut out = String::from("the graph has unsatisfied imports:");
        for g in v["gaps"].as_array().cloned().unwrap_or_default() {
            let cands: Vec<&str> =
                g["candidates"].as_array().map(|a| a.iter().filter_map(|c| c.as_str()).collect())
                    .unwrap_or_default();
            out.push_str(&format!(
                "\n  {} needs {}",
                g["component"].as_str().unwrap_or("?"),
                g["interface"].as_str().unwrap_or("?")
            ));
            out.push_str(&match cands.is_empty() {
                true => "\n      nothing in your catalogue exports it — upload a component that does"
                    .to_string(),
                false => format!("\n      wire one of: {}", cands.join(", ")),
            });
        }
        return out;
    }
    v["error"].as_str().map(String::from).unwrap_or_else(|| v.to_string())
}

pub fn login(url: &str, email: &str, password: &str) -> Result<()> {
    let anon = Session { url: url.to_string(), ..Default::default() };
    let body = json!({ "email": email, "password": password });
    let v = call(&anon, "POST", "/api/login", Some(body.to_string().into_bytes()), "application/json")?;
    let token = v["token"].as_str().unwrap_or_default().to_string();
    if token.is_empty() {
        bail!("the platform returned no token");
    }
    let s = Session {
        url: url.to_string(),
        token,
        tenant: v["tenant"].as_str().unwrap_or_default().to_string(),
    };
    save(&s)?;
    println!("logged in to {} as tenant {}", s.url, s.tenant);
    Ok(())
}

pub fn register(url: &str, email: &str, password: &str) -> Result<()> {
    let anon = Session { url: url.to_string(), ..Default::default() };
    let body = json!({ "email": email, "password": password });
    call(&anon, "POST", "/api/register", Some(body.to_string().into_bytes()), "application/json")?;
    println!("registered {email}");
    login(url, email, password)
}

pub fn whoami() -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", "/api/me", None, "application/json")?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

pub fn component_push(file: &PathBuf, id: Option<String>) -> Result<()> {
    let s = load()?;
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    // The default id is the filename, minus the `.composed` a `just compose-*` adds,
    // so `comp component push target/gate_domain.composed.wasm` does the obvious.
    let id = id.unwrap_or_else(|| {
        file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("component")
            .trim_end_matches(".composed")
            .replace('_', "-")
    });
    let v = call(&s, "POST", &format!("/api/components?id={id}"), Some(bytes), "application/wasm")?;
    println!("uploaded {id}");
    if let Some(surface) = v.get("surface") {
        let n = |k: &str| surface[k].as_array().map(|a| a.len()).unwrap_or(0);
        println!("  exports {} · imports {}", n("exports"), n("imports"));
    }
    println!("  it will be distributed to the lattice on the next reconcile pass");
    Ok(())
}

pub fn component_ls() -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", "/api/components", None, "application/json")?;
    let rows = v["components"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no components yet — `comp component push <file.wasm>`");
        return Ok(());
    }
    println!("{:<28} {:<10} {:<12} {}", "ID", "VISIBLE", "DISTRIBUTED", "EXPORTS");
    for r in rows {
        let digest = r["digest"].as_str().unwrap_or("");
        let exports: Vec<&str> = r["surface"]["exports"]
            .as_array()
            .map(|a| a.iter().filter_map(|e| e["raw"].as_str()).collect())
            .unwrap_or_default();
        println!(
            "{:<28} {:<10} {:<12} {}",
            r["id"].as_str().unwrap_or("?"),
            r["visibility"].as_str().unwrap_or("private"),
            if digest.is_empty() { "pending" } else { "yes" },
            exports.first().copied().unwrap_or("-")
        );
    }
    Ok(())
}

pub fn app_create(name: &str, strategy: &str, components: &[String], links: &[String], org: Option<&str>) -> Result<()> {
    let s = load()?;
    let edges: Result<Vec<Value>> = links
        .iter()
        .map(|l| {
            // `plug:socket:iface` — the same triple the canvas draws and the
            // reconciler turns into a link table.
            let parts: Vec<&str> = l.splitn(3, ':').collect();
            match parts.as_slice() {
                [plug, socket, iface] => Ok(json!({ "plug": plug, "socket": socket, "iface": iface })),
                _ => bail!("--link wants plug:socket:iface, got {l:?}"),
            }
        })
        .collect();
    let body = json!({
        "name": name, "strategy": strategy,
        "nodes": components, "edges": edges?,
    });
    // `?org=` selects whose deployment this is; omitted means the caller's own.
    let path = match org {
        Some(o) => format!("/api/deployments?org={o}"),
        None => "/api/deployments".to_string(),
    };
    let v = call(&s, "POST", &path, Some(body.to_string().into_bytes()), "application/json")?;
    println!("created {} ({})", v["name"].as_str().unwrap_or(name), v["id"].as_str().unwrap_or("?"));
    println!("  `comp app deploy {}` to save and place it", v["id"].as_str().unwrap_or("<id>"));
    Ok(())
}

pub fn app_deploy(id: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "POST", &format!("/api/deployments/{id}/save"), None, "application/json")?;
    println!(
        "revision {} of `{}` saved ({}, {} component(s))",
        v["revision"], v["app"].as_str().unwrap_or("?"), v["strategy"].as_str().unwrap_or("?"),
        v["components"]
    );
    println!("  reachable at {}", v["ingress"].as_str().unwrap_or("?"));
    println!("  the reconciler places it on the next pass");
    Ok(())
}

pub fn app_ls() -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", "/api/deployments", None, "application/json")?;
    let rows = v["deployments"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no deployments yet — `comp app create`");
        return Ok(());
    }
    println!("{:<26} {:<16} {:<10} {:<9} {}", "ID", "NAME", "STRATEGY", "REVISION", "STATUS");
    for r in rows {
        println!(
            "{:<26} {:<16} {:<10} {:<9} {}",
            r["id"].as_str().unwrap_or("?"),
            r["name"].as_str().unwrap_or("?"),
            r["strategy"].as_str().unwrap_or("?"),
            r["revision"],
            r["status"].as_str().unwrap_or("-")
        );
    }
    Ok(())
}

pub fn app_show(id: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", &format!("/api/deployments/{id}"), None, "application/json")?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

pub fn app_manifest(id: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", &format!("/api/deployments/{id}/manifests"), None, "application/json")?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

pub fn app_rm(id: &str, confirm: &str) -> Result<()> {
    let s = load()?;
    // The confirmation is the platform's rule (ADR-0016), not this tool's — it is
    // passed straight through so the refusal comes from the one place that knows
    // what deleting actually destroys.
    let v = call(
        &s,
        "DELETE",
        &format!("/api/deployments/{id}?confirm={confirm}"),
        None,
        "application/json",
    )?;
    println!("deleted {}", v["deleted"].as_str().unwrap_or(id));
    println!("  {}", v["note"].as_str().unwrap_or("the lattice stops it shortly"));
    Ok(())
}

// ---- organisations ---------------------------------------------------------

pub fn org_create(name: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "POST", "/api/orgs", Some(json!({ "name": name }).to_string().into_bytes()),
                 "application/json")?;
    println!("created org {} ({})", v["name"].as_str().unwrap_or(name), v["id"].as_str().unwrap_or("?"));
    println!("  `comp org invite {}` to add someone", v["id"].as_str().unwrap_or("<id>"));
    Ok(())
}

pub fn org_ls() -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", "/api/orgs", None, "application/json")?;
    let rows = v["orgs"].as_array().cloned().unwrap_or_default();
    println!("{:<24} {:<24} {}", "ID", "NAME", "YOUR ROLE");
    for r in rows {
        println!("{:<24} {:<24} {}", r["id"].as_str().unwrap_or("?"),
                 r["name"].as_str().unwrap_or("?"), r["role"].as_str().unwrap_or("?"));
    }
    Ok(())
}

pub fn org_invite(org: &str, role: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "POST", &format!("/api/orgs/{org}/invites"),
                 Some(json!({ "role": role }).to_string().into_bytes()), "application/json")?;
    println!("invite code: {}", v["code"].as_str().unwrap_or("?"));
    println!("  role {} · single use · redeem with `comp org join <code>`",
             v["role"].as_str().unwrap_or(role));
    Ok(())
}

pub fn org_join(code: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "POST", "/api/orgs/join",
                 Some(json!({ "code": code }).to_string().into_bytes()), "application/json")?;
    println!("joined {} as {}", v["org"].as_str().unwrap_or("?"), v["role"].as_str().unwrap_or("?"));
    Ok(())
}

pub fn org_members(org: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", &format!("/api/orgs/{org}/members"), None, "application/json")?;
    println!("{:<30} {}", "SUBJECT", "ROLE");
    for m in v["members"].as_array().cloned().unwrap_or_default() {
        println!("{:<30} {}", m["subject"].as_str().unwrap_or("?"), m["role"].as_str().unwrap_or("?"));
    }
    Ok(())
}

pub fn org_remove(org: &str, subject: &str) -> Result<()> {
    let s = load()?;
    call(&s, "DELETE", &format!("/api/orgs/{org}/members/{subject}"), None, "application/json")?;
    println!("removed {subject} from {org}");
    Ok(())
}

// ---- secrets ---------------------------------------------------------------

/// Store a secret, reading the VALUE from a file or stdin — never from argv.
///
/// A token on a command line is a token in `~/.bash_history`, in `ps` output for
/// every other user on the box, and in the shell's own recall. Those are three
/// disclosures that no amount of care at the platform end can undo, so this tool
/// will not accept one that way.
///
/// The reference it prints is what a manifest carries (ADR-0010): the manifest
/// gets a pointer, never the value.
pub fn secret_set(name: &str, from: Option<&PathBuf>, org: Option<&str>) -> Result<()> {
    let s = load()?;
    let value = match from {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading {}", p.display()))?,
        // Not a terminal: something is piping. Read it and stay silent, so this
        // composes in a script.
        None if !stdin_is_a_terminal() => {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                .context("reading the value from stdin")?;
            buf
        }
        // A person is here. Ask, with the echo off — a key pasted into a visible
        // prompt is a key in the scrollback, on the screen behind them, and in
        // any terminal recording.
        None => {
            let first = rpassword::prompt_password(format!("value for {name}: "))
                .context("reading the value")?;
            let again = rpassword::prompt_password("again: ").context("reading the confirmation")?;
            if first != again {
                // Worth confirming precisely BECAUSE it is hidden: a mistyped key
                // that nobody can see fails later, at a provider, with a message
                // about authentication rather than about typing.
                anyhow::bail!("the two values differ — nothing was stored");
            }
            first
        }
    };
    // Trailing newlines are what `echo` and every editor add, and a bearer token
    // with one on the end fails authentication in a way whose error message says
    // nothing about a newline.
    let value = value.trim_end_matches(['\n', '\r']).to_string();
    if value.is_empty() {
        anyhow::bail!("the value is empty — nothing was stored");
    }
    let path = match org {
        Some(o) => format!("/api/secrets?org={o}"),
        None => "/api/secrets".to_string(),
    };
    let v = call(&s, "POST", &path, Some(json!({ "name": name, "value": value }).to_string().into_bytes()),
                 "application/json")?;
    let reference = v["ref"].as_str().unwrap_or("?");
    println!("stored {reference} (version {})", v["version"].as_str().unwrap_or("?"));
    println!("  grant it to a component with:");
    println!("    secrets:");
    println!("      - key: {name}");
    println!("        ref: {reference}");
    Ok(())
}

/// Names, never values. There is no endpoint that returns one.
pub fn secret_ls(org: Option<&str>) -> Result<()> {
    let s = load()?;
    let path = match org {
        Some(o) => format!("/api/secrets?org={o}"),
        None => "/api/secrets".to_string(),
    };
    let v = call(&s, "GET", &path, None, "application/json")?;
    let rows = v["secrets"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no secrets — `comp secret set <name> --from <file>` stores one");
        return Ok(());
    }
    println!("{:<28} {}", "NAME", "REFERENCE");
    for r in rows {
        println!("{:<28} {}", r["name"].as_str().unwrap_or("?"), r["ref"].as_str().unwrap_or("?"));
    }
    Ok(())
}

pub fn secret_rm(name: &str, org: Option<&str>) -> Result<()> {
    let s = load()?;
    let path = match org {
        Some(o) => format!("/api/secrets/{name}?org={o}"),
        None => format!("/api/secrets/{name}"),
    };
    call(&s, "DELETE", &path, None, "application/json")?;
    // Deliberately blunt: an instance granted this reference will fail to START
    // on its next reconcile, rather than failing on its first request (ADR-0051).
    println!("deleted {name} — any component granted it will stop starting");
    Ok(())
}

/// Is stdin a terminal? Decides between prompting and reading a pipe.
fn stdin_is_a_terminal() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: isatty on a valid fd is a read-only query and cannot fail badly.
        unsafe { libc_isatty(std::io::stdin().as_raw_fd()) == 1 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "isatty"]
    fn libc_isatty(fd: i32) -> i32;
}

// ---- projects and goals (ADR-0082) -----------------------------------------

pub fn project_add(name: &str, repo: &str, base: &str, org: Option<&str>) -> Result<()> {
    let s = load()?;
    let path = match org {
        Some(o) => format!("/api/projects?org={o}"),
        None => "/api/projects".to_string(),
    };
    let v = call(&s, "POST", &path,
        Some(json!({ "name": name, "repo": repo, "base": base }).to_string().into_bytes()),
        "application/json")?;
    println!("created project {} -> {} ({})",
        v["name"].as_str().unwrap_or(name),
        v["repo"].as_str().unwrap_or(repo),
        v["base"].as_str().unwrap_or(base));
    println!("  grant it a forge token and a model key:");
    println!("    comp secret set forge-token");
    println!("    comp secret set openai-api-key");
    println!("  then queue something: `comp goal add {name} \"...\"`");
    Ok(())
}

pub fn project_ls(org: Option<&str>) -> Result<()> {
    let s = load()?;
    let path = match org {
        Some(o) => format!("/api/projects?org={o}"),
        None => "/api/projects".to_string(),
    };
    let v = call(&s, "GET", &path, None, "application/json")?;
    let rows = v["projects"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no projects — `comp project add <name> --repo owner/name` starts one");
        return Ok(());
    }
    println!("{:<20} {:<28} {:<8} {:>6} {:>7} {:>6}", "NAME", "REPO", "BASE", "QUEUED", "RUNNING", "FAILED");
    for r in rows {
        println!("{:<20} {:<28} {:<8} {:>6} {:>7} {:>6}",
            r["name"].as_str().unwrap_or("?"),
            r["repo"].as_str().unwrap_or("?"),
            r["base"].as_str().unwrap_or("?"),
            r["queued"].as_u64().unwrap_or(0),
            r["running"].as_u64().unwrap_or(0),
            r["failed"].as_u64().unwrap_or(0));
    }
    Ok(())
}

pub fn goal_add(project: &str, title: &str, spec: Option<&str>, priority: Option<i64>) -> Result<()> {
    let s = load()?;
    let mut body = json!({ "title": title });
    if let Some(p) = spec {
        body["spec"] = json!(p);
    }
    if let Some(p) = priority {
        body["priority"] = json!(p);
    }
    let v = call(&s, "POST", &format!("/api/projects/{project}/goals"),
        Some(body.to_string().into_bytes()), "application/json")?;
    let id = v["id"].as_str().unwrap_or("?");
    println!("queued {id}: {}", v["title"].as_str().unwrap_or(title));
    // Nothing drains this queue. Saying so here is the difference between a
    // person waiting for something that will never happen and a person starting
    // it (ADR-0082).
    println!("  nothing starts it on its own — `comp goal start {id}` when you are ready");
    Ok(())
}

pub fn goal_ls(project: &str, state: Option<&str>) -> Result<()> {
    let s = load()?;
    let path = match state {
        Some(st) => format!("/api/projects/{project}/goals?state={st}"),
        None => format!("/api/projects/{project}/goals"),
    };
    let v = call(&s, "GET", &path, None, "application/json")?;
    let rows = v["goals"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no goals — `comp goal add {project} \"what to do\"` queues one");
        return Ok(());
    }
    println!("{:<28} {:<16} {:>4}  {}", "ID", "STATE", "PRI", "TITLE");
    for r in rows {
        let title = r["title"].as_str().unwrap_or("?");
        let reason = r["reason"].as_str().unwrap_or_default();
        println!("{:<28} {:<16} {:>4}  {title}{}",
            r["id"].as_str().unwrap_or("?"),
            r["state"].as_str().unwrap_or("?"),
            r["priority"].as_i64().unwrap_or(100),
            if reason.is_empty() { String::new() } else { format!("  ({reason})") });
    }
    Ok(())
}

/// The one transition a person must make for work to happen.
pub fn goal_start(id: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "POST", &format!("/api/goals/{id}/start"), Some(b"{}".to_vec()), "application/json")?;
    println!("started {id}: {}", v["title"].as_str().unwrap_or("?"));
    Ok(())
}

pub fn goal_fail(id: &str, reason: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "POST", &format!("/api/goals/{id}/fail"),
        Some(json!({ "reason": reason }).to_string().into_bytes()), "application/json")?;
    println!("failed {id}: {}", v["title"].as_str().unwrap_or("?"));
    // A dead letter is terminal on purpose: re-running an LLM goal unchanged
    // costs money and fails the same way. A retry is a NEW goal, so the history
    // of what was tried stays honest.
    println!("  it stays in the dead-letter queue — requeue by adding a new goal");
    Ok(())
}

pub fn goal_abandon(id: &str) -> Result<()> {
    let s = load()?;
    call(&s, "DELETE", &format!("/api/goals/{id}"), None, "application/json")?;
    println!("abandoned {id}");
    Ok(())
}
