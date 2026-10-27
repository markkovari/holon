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
    std::env::var("HOLON_CREDENTIALS")
        .or_else(|_| std::env::var("COMP_CREDENTIALS"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            let holon_path = PathBuf::from(&home).join(".config/holon/credentials.json");
            if holon_path.exists() {
                holon_path
            } else {
                PathBuf::from(home).join(".config/comp/credentials.json")
            }
        })
}

pub fn load() -> Result<Session> {
    let p = creds_path();
    let raw = std::fs::read(&p)
        .with_context(|| format!("no session at {} — run `holon login` first", p.display()))?;
    serde_json::from_slice(&raw).context("credentials file is not readable JSON")
}

fn save(s: &Session) -> Result<()> {
    let p = creds_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&p, serde_json::to_vec_pretty(s)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

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

fn explain(v: &Value) -> String {
    if v["error"] == json!("unsatisfied_imports") {
        let mut out = String::from("the graph has unsatisfied imports:");
        for g in v["gaps"].as_array().cloned().unwrap_or_default() {
            let cands: Vec<&str> = g["candidates"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| c.as_str()).collect())
                .unwrap_or_default();
            out.push_str(&format!(
                "\n  {} needs {}",
                g["component"].as_str().unwrap_or("?"),
                g["interface"].as_str().unwrap_or("?")
            ));
            out.push_str(&match cands.is_empty() {
                true => {
                    "\n      nothing in your catalogue exports it — upload a component that does"
                        .to_string()
                }
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
    let v =
        call(&anon, "POST", "/api/login", Some(body.to_string().into_bytes()), "application/json")?;
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
    // Strip the `.composed` that `cargo xtask compose` adds.
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
    println!("{:<28} {:<10} {:<12} EXPORTS", "ID", "VISIBLE", "DISTRIBUTED");
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

pub fn app_create(
    name: &str,
    strategy: &str,
    components: &[String],
    links: &[String],
    org: Option<&str>,
) -> Result<()> {
    let s = load()?;
    let edges: Result<Vec<Value>> = links
        .iter()
        .map(|l| {
            let parts: Vec<&str> = l.splitn(3, ':').collect();
            match parts.as_slice() {
                [plug, socket, iface] => {
                    Ok(json!({ "plug": plug, "socket": socket, "iface": iface }))
                }
                _ => bail!("--link wants plug:socket:iface, got {l:?}"),
            }
        })
        .collect();
    let body = json!({
        "name": name, "strategy": strategy,
        "nodes": components, "edges": edges?,
    });
    let path = match org {
        Some(o) => format!("/api/deployments?org={o}"),
        None => "/api/deployments".to_string(),
    };
    let v = call(&s, "POST", &path, Some(body.to_string().into_bytes()), "application/json")?;
    println!(
        "created {} ({})",
        v["name"].as_str().unwrap_or(name),
        v["id"].as_str().unwrap_or("?")
    );
    println!("  `comp app deploy {}` to save and place it", v["id"].as_str().unwrap_or("<id>"));
    Ok(())
}

pub fn app_deploy(id: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "POST", &format!("/api/deployments/{id}/save"), None, "application/json")?;
    println!(
        "revision {} of `{}` saved ({}, {} component(s))",
        v["revision"],
        v["app"].as_str().unwrap_or("?"),
        v["strategy"].as_str().unwrap_or("?"),
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
    println!("{:<26} {:<16} {:<10} {:<9} STATUS", "ID", "NAME", "STRATEGY", "REVISION");
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
    // The platform owns the confirmation rule (ADR-0016); pass it straight through.
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

pub fn org_create(name: &str) -> Result<()> {
    let s = load()?;
    let v = call(
        &s,
        "POST",
        "/api/orgs",
        Some(json!({ "name": name }).to_string().into_bytes()),
        "application/json",
    )?;
    println!(
        "created org {} ({})",
        v["name"].as_str().unwrap_or(name),
        v["id"].as_str().unwrap_or("?")
    );
    println!("  `comp org invite {}` to add someone", v["id"].as_str().unwrap_or("<id>"));
    Ok(())
}

pub fn org_ls() -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", "/api/orgs", None, "application/json")?;
    let rows = v["orgs"].as_array().cloned().unwrap_or_default();
    println!("{:<24} {:<24} YOUR ROLE", "ID", "NAME");
    for r in rows {
        println!(
            "{:<24} {:<24} {}",
            r["id"].as_str().unwrap_or("?"),
            r["name"].as_str().unwrap_or("?"),
            r["role"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

pub fn org_invite(org: &str, role: &str) -> Result<()> {
    let s = load()?;
    let v = call(
        &s,
        "POST",
        &format!("/api/orgs/{org}/invites"),
        Some(json!({ "role": role }).to_string().into_bytes()),
        "application/json",
    )?;
    println!("invite code: {}", v["code"].as_str().unwrap_or("?"));
    println!(
        "  role {} · single use · redeem with `comp org join <code>`",
        v["role"].as_str().unwrap_or(role)
    );
    Ok(())
}

pub fn org_join(code: &str) -> Result<()> {
    let s = load()?;
    let v = call(
        &s,
        "POST",
        "/api/orgs/join",
        Some(json!({ "code": code }).to_string().into_bytes()),
        "application/json",
    )?;
    println!(
        "joined {} as {}",
        v["org"].as_str().unwrap_or("?"),
        v["role"].as_str().unwrap_or("?")
    );
    Ok(())
}

pub fn org_members(org: &str) -> Result<()> {
    let s = load()?;
    let v = call(&s, "GET", &format!("/api/orgs/{org}/members"), None, "application/json")?;
    println!("{:<30} ROLE", "SUBJECT");
    for m in v["members"].as_array().cloned().unwrap_or_default() {
        println!(
            "{:<30} {}",
            m["subject"].as_str().unwrap_or("?"),
            m["role"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

pub fn org_remove(org: &str, subject: &str) -> Result<()> {
    let s = load()?;
    call(&s, "DELETE", &format!("/api/orgs/{org}/members/{subject}"), None, "application/json")?;
    println!("removed {subject} from {org}");
    Ok(())
}

/// The value comes from a file or stdin, never argv (shell history, `ps`).
pub fn secret_set(name: &str, from: Option<&PathBuf>, org: Option<&str>) -> Result<()> {
    let s = load()?;
    let value = match from {
        Some(p) => {
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
        }
        // Piped: read silently so this composes in a script.
        None if !stdin_is_a_terminal() => {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                .context("reading the value from stdin")?;
            buf
        }
        // Echo off: a pasted key would otherwise land in scrollback and recordings.
        None => {
            let first = rpassword::prompt_password(format!("value for {name}: "))
                .context("reading the value")?;
            let again =
                rpassword::prompt_password("again: ").context("reading the confirmation")?;
            if first != again {
                // Confirmed because hidden: a mistyped key fails later as an auth error.
                anyhow::bail!("the two values differ — nothing was stored");
            }
            first
        }
    };
    // `echo` and editors add a newline, which then fails auth with no hint why.
    let value = value.trim_end_matches(['\n', '\r']).to_string();
    if value.is_empty() {
        anyhow::bail!("the value is empty — nothing was stored");
    }
    let path = match org {
        Some(o) => format!("/api/secrets?org={o}"),
        None => "/api/secrets".to_string(),
    };
    let v = call(
        &s,
        "POST",
        &path,
        Some(json!({ "name": name, "value": value }).to_string().into_bytes()),
        "application/json",
    )?;
    let reference = v["ref"].as_str().unwrap_or("?");
    println!("stored {reference} (version {})", v["version"].as_str().unwrap_or("?"));
    println!("  grant it to a component with:");
    println!("    secrets:");
    println!("      - key: {name}");
    println!("        ref: {reference}");
    Ok(())
}

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
    println!("{:<28} REFERENCE", "NAME");
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
    // Blunt on purpose: a granted instance fails to start, not at first request (ADR-0051).
    println!("deleted {name} — any component granted it will stop starting");
    Ok(())
}

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

pub fn project_add(name: &str, repo: &str, base: &str, org: Option<&str>) -> Result<()> {
    let s = load()?;
    let path = match org {
        Some(o) => format!("/api/projects?org={o}"),
        None => "/api/projects".to_string(),
    };
    let v = call(
        &s,
        "POST",
        &path,
        Some(json!({ "name": name, "repo": repo, "base": base }).to_string().into_bytes()),
        "application/json",
    )?;
    println!(
        "created project {} -> {} ({})",
        v["name"].as_str().unwrap_or(name),
        v["repo"].as_str().unwrap_or(repo),
        v["base"].as_str().unwrap_or(base)
    );
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
    println!(
        "{:<20} {:<28} {:<8} {:>6} {:>7} {:>6}",
        "NAME", "REPO", "BASE", "QUEUED", "RUNNING", "FAILED"
    );
    for r in rows {
        println!(
            "{:<20} {:<28} {:<8} {:>6} {:>7} {:>6}",
            r["name"].as_str().unwrap_or("?"),
            r["repo"].as_str().unwrap_or("?"),
            r["base"].as_str().unwrap_or("?"),
            r["queued"].as_u64().unwrap_or(0),
            r["running"].as_u64().unwrap_or(0),
            r["failed"].as_u64().unwrap_or(0)
        );
    }
    Ok(())
}

pub fn goal_add(
    project: &str,
    title: &str,
    spec: Option<&str>,
    priority: Option<i64>,
) -> Result<()> {
    let s = load()?;
    let mut body = json!({ "title": title });
    if let Some(p) = spec {
        body["spec"] = json!(p);
    }
    if let Some(p) = priority {
        body["priority"] = json!(p);
    }
    let v = call(
        &s,
        "POST",
        &format!("/api/projects/{project}/goals"),
        Some(body.to_string().into_bytes()),
        "application/json",
    )?;
    let id = v["id"].as_str().unwrap_or("?");
    println!("queued {id}: {}", v["title"].as_str().unwrap_or(title));
    println!("  nothing starts it on its own — `comp goal start {id}` when you are ready");
    Ok(())
}

fn print_goals(project: &str, rows: &[Value]) {
    if rows.is_empty() {
        println!("no goals — `comp goal add {project} \"what to do\"` queues one");
        return;
    }
    println!("{:<28} {:<16} {:>4}  TITLE", "ID", "STATE", "PRI");
    for r in rows {
        let title = r["title"].as_str().unwrap_or("?");
        let reason = r["reason"].as_str().unwrap_or_default();
        let pr = r["pr"].as_str().unwrap_or_default();
        let suffix = if !reason.is_empty() {
            format!("  ({reason})")
        } else if !pr.is_empty() {
            format!("  ({pr})")
        } else {
            String::new()
        };
        println!(
            "{:<28} {:<16} {:>4}  {title}{suffix}",
            r["id"].as_str().unwrap_or("?"),
            r["state"].as_str().unwrap_or("?"),
            r["priority"].as_i64().unwrap_or(100),
        );
    }
}

pub fn goal_ls(project: &str, state: Option<&str>) -> Result<()> {
    let s = load()?;
    let path = match state {
        Some(st) => format!("/api/projects/{project}/goals?state={st}"),
        None => format!("/api/projects/{project}/goals"),
    };
    let v = call(&s, "GET", &path, None, "application/json")?;
    print_goals(project, &v["goals"].as_array().cloned().unwrap_or_default());
    Ok(())
}

/// Events only decide how soon to refresh, never what to show; a per-process consumer
/// group keeps concurrent watchers from stealing each other's events.
pub fn goal_watch(project: &str, interval: u64, full_refresh: u64) -> Result<()> {
    let s = load()?;
    let group = format!("watch-{}", std::process::id());
    let mut last_full = std::time::Instant::now();

    let refresh = |s: &Session| -> Result<()> {
        let v =
            call(s, "GET", &format!("/api/projects/{project}/goals"), None, "application/json")?;
        println!("\n[{}]", now_hms());
        print_goals(project, &v["goals"].as_array().cloned().unwrap_or_default());
        Ok(())
    };
    refresh(&s)?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(interval));
        let events = call(
            &s,
            "GET",
            &format!("/api/projects/{project}/events?group={group}&max=50"),
            None,
            "application/json",
        )
        .ok()
        .and_then(|v| v["events"].as_array().cloned())
        .unwrap_or_default();

        if !events.is_empty() {
            for e in &events {
                let ev = &e["event"];
                println!(
                    "[{}] {} {} -> {}",
                    now_hms(),
                    ev["id"].as_str().unwrap_or("?"),
                    ev["from"].as_str().unwrap_or("(new)"),
                    ev["to"].as_str().unwrap_or("?"),
                );
            }
            let ids: Vec<Value> =
                events.iter().map(|e| json!(e["id"].as_str().unwrap_or_default())).collect();
            let _ = call(
                &s,
                "POST",
                &format!("/api/projects/{project}/events/ack"),
                Some(json!({ "group": group, "ids": ids }).to_string().into_bytes()),
                "application/json",
            );
            refresh(&s)?;
            last_full = std::time::Instant::now();
        } else if last_full.elapsed().as_secs() >= full_refresh {
            // Safety net: a dropped event or bus outage must not leave the view stale.
            refresh(&s)?;
            last_full = std::time::Instant::now();
        }
    }
}

fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        % 86_400;
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

pub fn goal_start(id: &str) -> Result<()> {
    let s = load()?;
    let v = call(
        &s,
        "POST",
        &format!("/api/goals/{id}/start"),
        Some(b"{}".to_vec()),
        "application/json",
    )?;
    println!("started {id}: {}", v["title"].as_str().unwrap_or("?"));
    Ok(())
}

pub fn goal_fail(id: &str, reason: &str) -> Result<()> {
    let s = load()?;
    let v = call(
        &s,
        "POST",
        &format!("/api/goals/{id}/fail"),
        Some(json!({ "reason": reason }).to_string().into_bytes()),
        "application/json",
    )?;
    println!("failed {id}: {}", v["title"].as_str().unwrap_or("?"));
    // Terminal on purpose: re-running an LLM goal unchanged costs money and fails the same way.
    println!("  it stays in the dead-letter queue — requeue by adding a new goal");
    Ok(())
}

pub fn goal_abandon(id: &str) -> Result<()> {
    let s = load()?;
    call(&s, "DELETE", &format!("/api/goals/{id}"), None, "application/json")?;
    println!("abandoned {id}");
    Ok(())
}
