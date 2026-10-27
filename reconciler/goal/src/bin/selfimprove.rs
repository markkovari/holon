//! Promotes a candidate over the running baseline only if healthy and strictly more capable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use holon_fleet::fleet::{repo_root, Fleet};
use semver::Version;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(
    name = "comp-selfimprove",
    about = "Promote a component version only if it is healthy and more capable, judged over the lattice."
)]
struct Args {
    /// The self-target component; must report `{healthy, capability_count, capabilities}`.
    #[arg(long, default_value = "version-probe")]
    component: String,
    /// The baseline's advertised capabilities (comma-separated). Ignored with `--baseline-src`.
    #[arg(long)]
    baseline: Option<String>,
    /// The candidate's advertised capabilities (comma-separated). Ignored with `--candidate-src`.
    #[arg(long)]
    candidate: Option<String>,
    /// A source tree to build the baseline from; its own manifest is what the bytes report.
    #[arg(long)]
    baseline_src: Option<PathBuf>,
    /// A source tree to build the candidate component FROM.
    #[arg(long)]
    candidate_src: Option<PathBuf>,
    /// Judge by behavior: call both versions with conformance cases; promote on a strict superset.
    #[arg(long, default_value_t = false)]
    behavior: bool,
    /// The conformance spec for --behavior (`<route> => <field>=<expected>` lines).
    #[arg(long)]
    cases: Option<PathBuf>,
}

/// The feature flag gives different bytes, a swap the fleet can see.
fn build_composed_slug(feature: bool) -> Result<Vec<u8>> {
    let comp = repo_root().join("components");
    let rel = comp.join("target/wasm32-wasip2/release");
    // slug-probe is p3: bindings come from `generate!`, so no cargo-component step.
    let mut build = Command::new("cargo");
    build.current_dir(&comp).args([
        "build",
        "--release",
        "--target",
        "wasm32-wasip2",
        "-p",
        "slug-probe",
        "-p",
        "slug",
    ]);
    if feature {
        build.env("COMP_SLUGWITH", "1");
    } else {
        build.env_remove("COMP_SLUGWITH");
    }
    let out = build.output().context("cargo build")?;
    if !out.status.success() {
        bail!("building slug-probe failed:\n{}", String::from_utf8_lossy(&out.stderr));
    }
    // The plug list is derived from what slug-probe imports (ADR-0087).
    let catalog = holon_fleet::plug::Catalog::scan(std::slice::from_ref(&rel));
    holon_fleet::plug::compose("slug-probe", &catalog)
        .map_err(|e| anyhow::anyhow!("composing slug-probe: {e}"))
}

struct Case {
    path: String,
    field: String,
    expected: String,
}

/// Each non-comment line is `<route> => <field>=<expected>`.
fn load_cases(path: &Path) -> Result<Vec<Case>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut cases = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (route, rhs) =
            line.split_once("=>").with_context(|| format!("case has no `=>`: {line}"))?;
        let (field, expected) =
            rhs.trim().split_once('=').with_context(|| format!("rhs is not field=value: {rhs}"))?;
        cases.push(Case {
            path: route.trim().to_string(),
            field: field.trim().to_string(),
            expected: expected.trim().to_string(),
        });
    }
    if cases.is_empty() {
        bail!("no conformance cases in {}", path.display());
    }
    Ok(cases)
}

fn call_json(fleet: &Fleet, host: &str, path: &str) -> Option<Value> {
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().ok()?;
    let r = http
        .get(format!("http://127.0.0.1:{}{path}", fleet.ingress_port))
        .header("host", host)
        .send()
        .ok()?;
    serde_json::from_str(&r.text().ok()?).ok()
}

/// A route it does not serve fails.
fn passing(fleet: &Fleet, host: &str, cases: &[Case]) -> Vec<usize> {
    cases
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            call_json(fleet, host, &c.path)
                .and_then(|v| v[&c.field].as_str().map(str::to_string))
                .as_deref()
                == Some(c.expected.as_str())
        })
        .map(|(i, _)| i)
        .collect()
}

fn behavior_gate(cases_path: &Path) -> Result<()> {
    let (id, host) = ("cap", "cap.improver.test");
    let cases = load_cases(cases_path)?;
    println!(
        "comp-selfimprove: judging by BEHAVIOR over the lattice — {} cases from {}\n",
        cases.len(),
        cases_path.display()
    );

    let base = build_composed_slug(false)?;
    let cand = build_composed_slug(true)?;
    if base == cand {
        bail!("baseline and candidate are identical bytes — nothing changed");
    }

    let fleet = Fleet::start_with_platform("behavior", 1);
    let api = Api::new(fleet.platform_url())?;
    let mut dep_id: Option<String> = None;

    deploy_capability(&api, &fleet, id, &mut dep_id, host, base, &cases, None)?;
    let base_pass = passing(&fleet, host, &cases);
    println!("  baseline  · passes {:?} of {} cases over the lattice", base_pass, cases.len());

    deploy_capability(&api, &fleet, id, &mut dep_id, host, cand, &cases, Some(&base_pass))?;
    let cand_pass = passing(&fleet, host, &cases);
    println!("  candidate · passes {:?} of {} cases over the lattice", cand_pass, cases.len());

    let regressed: Vec<usize> =
        base_pass.iter().copied().filter(|i| !cand_pass.contains(i)).collect();
    let gained: Vec<usize> = cand_pass.iter().copied().filter(|i| !base_pass.contains(i)).collect();
    println!();
    if regressed.is_empty() && !gained.is_empty() {
        println!("  PROMOTE — the candidate handles cases {gained:?} the baseline could not, and regressed none.");
        println!("  judged by calling the running code over the lattice, not by a report.");
        Ok(())
    } else {
        let why = if !regressed.is_empty() {
            format!("regressed cases {regressed:?}")
        } else {
            "handled no case the baseline could not".to_string()
        };
        println!("  ROLL BACK — the candidate {why}.");
        bail!("candidate rejected: {why}");
    }
}

/// Live means it passes some case (`prev` None), or passes other than `prev` (the swap landed).
// Each is a distinct deploy-and-wait parameter, not a struct's fields waiting to be extracted.
#[allow(clippy::too_many_arguments)]
fn deploy_capability(
    api: &Api,
    fleet: &Fleet,
    id: &str,
    dep_id: &mut Option<String>,
    host: &str,
    wasm: Vec<u8>,
    cases: &[Case],
    prev: Option<&Vec<usize>>,
) -> Result<()> {
    api.upload(id, wasm)?;
    if dep_id.is_none() {
        let (code, dep) =
            api.post("/api/deployments", json!({ "name": id, "nodes": [{"id": id}], "edges": [] }));
        if code != 201 {
            bail!("deploy create failed: {dep}");
        }
        *dep_id = Some(dep["id"].as_str().unwrap_or_default().to_string());
    }
    let did = dep_id.clone().unwrap();
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        let _ = api.post(&format!("/api/deployments/{did}/save"), json!({}));
        let now = passing(fleet, host, cases);
        let ready = match prev {
            None => !now.is_empty(),
            Some(before) => &now != before,
        };
        if ready {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    bail!("the capability never went live over the lattice\n{}", fleet.node_log("n1"))
}

/// Only the tag is baked in, so two versions are different bytes; capabilities arrive as config.
fn build_probe(components: &Path, component: &str, tag: &str) -> Result<Vec<u8>> {
    // cargo-component only generates bindings (it hardcodes wasip1); the build targets wasip2.
    // A p3 crate makes its bindings at compile time and must skip it.
    let manifest =
        std::fs::read_to_string(components.join(component).join("Cargo.toml")).unwrap_or_default();
    let p3 = manifest.lines().any(|l| l.trim_start().starts_with("wit-bindgen ="));
    let chk = if p3 {
        Command::new("true").output()
    } else {
        Command::new("cargo")
            .current_dir(components)
            .args(["component", "check", "--release", "-p", component])
            .output()
    }
    .context("cargo component check (bindings)")?;
    if !chk.status.success() {
        bail!(
            "generating bindings for {component} failed:\n{}",
            String::from_utf8_lossy(&chk.stderr)
        );
    }
    let mut cmd = Command::new("cargo");
    cmd.current_dir(components)
        .args(["build", "--release", "--target", "wasm32-wasip2", "-p", component])
        .env("COMP_VERSION_TAG", tag)
        .env_remove("COMP_CAPS");
    let out = cmd.output().context("running cargo")?;
    if !out.status.success() {
        bail!("building {tag} failed:\n{}", String::from_utf8_lossy(&out.stderr));
    }
    let wasm = component.replace('-', "_");
    let path = components.join(format!("target/wasm32-wasip2/release/{wasm}.wasm"));
    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
}

struct Api {
    base: String,
    http: reqwest::blocking::Client,
    token: String,
}

impl Api {
    fn new(base: String) -> Result<Self> {
        let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build()?;
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            if http.get(&base).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let cred = json!({ "email": "improver@holon.test", "password": "password123" });
        let reg = http.post(format!("{base}/api/register")).json(&cred).send();
        let _ = reg;
        let v: Value = http
            .post(format!("{base}/api/login"))
            .json(&cred)
            .send()?
            .json()
            .unwrap_or(Value::Null);
        let token = v["token"].as_str().unwrap_or_default().to_string();
        if token.is_empty() {
            bail!("could not log in to the platform: {v}");
        }
        Ok(Self { base, http, token })
    }

    fn upload(&self, id: &str, wasm: Vec<u8>) -> Result<()> {
        // A component accepts only config keys its uploader declared (ADR-0047).
        let code = self
            .http
            .post(format!("{}/api/components?id={id}&config=capabilities", self.base))
            .bearer_auth(&self.token)
            .body(wasm)
            .send()?
            .status()
            .as_u16();
        if !matches!(code, 200 | 201) {
            bail!("upload of {id} returned {code}");
        }
        Ok(())
    }

    fn post(&self, path: &str, body: Value) -> (u16, Value) {
        match self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
        {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or(Value::Null)),
            Err(e) => (0, Value::String(format!("transport: {e}"))),
        }
    }
}

fn report(fleet: &Fleet, host: &str) -> Option<Value> {
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().ok()?;
    let r = http
        .get(format!("http://127.0.0.1:{}/", fleet.ingress_port))
        .header("host", host)
        .send()
        .ok()?;
    serde_json::from_str(&r.text().ok()?).ok()
}

/// `caps` rides in node config on create and save, so a newer version updates the registry.
// See `deploy_capability`'s note.
#[allow(clippy::too_many_arguments)]
fn deploy_and_read(
    api: &Api,
    fleet: &Fleet,
    id: &str,
    dep_id: &mut Option<String>,
    host: &str,
    wasm: Vec<u8>,
    want: &str,
    caps: &str,
) -> Result<Value> {
    api.upload(id, wasm)?;
    let node = json!({ "id": id, "config": { "capabilities": caps } });
    if dep_id.is_none() {
        let (code, dep) = api
            .post("/api/deployments", json!({ "name": id, "nodes": [node.clone()], "edges": [] }));
        if code != 201 {
            bail!("deploy create failed: {dep}");
        }
        *dep_id = Some(dep["id"].as_str().unwrap_or_default().to_string());
    }
    let did = dep_id.clone().unwrap();
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut last_save = Value::Null;
    while Instant::now() < deadline {
        // Retried: an upload clears the digest and the push pass must stage it first (ADR-0006).
        let (sc, sb) =
            api.post(&format!("/api/deployments/{did}/save"), json!({ "nodes": [node.clone()] }));
        last_save = json!({ "code": sc, "body": sb });
        let r = report(fleet, host);
        let tag_ok = r.as_ref().and_then(|r| r["tag"].as_str()) == Some(want);
        let caps_seen = r.as_ref().map(|r| !caps_of(r).is_empty()).unwrap_or(false);
        if tag_ok && caps_seen {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    report(fleet, host).with_context(|| {
        let rec = fleet.reconciler_log();
        let tail: String = rec.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        format!("{want} never answered\n--- last save ---\n{last_save}\n--- reconciler (tail) ---\n{tail}")
    })
}

fn registry_caps(tree: &Path) -> Result<String> {
    let path = tree.join("components/capman/capabilities.txt");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join(","))
}

fn caps_of(report: &Value) -> BTreeMap<String, String> {
    report["capabilities"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect()
}

/// Unparseable sorts as `0.0.0`, so a malformed version reads as a downgrade: the safe direction.
fn ver(s: &str) -> Version {
    Version::parse(s.trim()).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn show(caps: &BTreeMap<String, String>) -> String {
    let parts: Vec<String> = caps.iter().map(|(n, v)| format!("{n}@{v}")).collect();
    format!("[{}]", parts.join(", "))
}

struct Verdict {
    regressions: Vec<String>,
    improvements: Vec<String>,
}

fn compare(base: &BTreeMap<String, String>, cand: &BTreeMap<String, String>) -> Verdict {
    let mut regressions = Vec::new();
    for (name, bv) in base {
        match cand.get(name) {
            None => regressions.push(format!("{name} removed")),
            Some(cv) if ver(cv) < ver(bv) => regressions.push(format!("{name} {bv}→{cv}")),
            _ => {}
        }
    }
    let mut improvements = Vec::new();
    for (name, cv) in cand {
        match base.get(name) {
            None => improvements.push(format!("{name}@{cv} (new)")),
            Some(bv) if ver(cv) > ver(bv) => improvements.push(format!("{name} {bv}→{cv}")),
            _ => {}
        }
    }
    Verdict { regressions, improvements }
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.behavior {
        let cases = args
            .cases
            .clone()
            .unwrap_or_else(|| repo_root().join("components/slug-probe/conformance.txt"));
        return behavior_gate(&cases);
    }
    let id = "self";
    let host = "self.improver.test";

    println!("comp-selfimprove: judging a candidate of `{}` over the lattice\n", args.component);

    let here = repo_root().join("components");
    let base_dir =
        args.baseline_src.clone().map(|d| d.join("components")).unwrap_or_else(|| here.clone());
    let cand_dir =
        args.candidate_src.clone().map(|d| d.join("components")).unwrap_or_else(|| here.clone());
    let base_registry = match &args.baseline_src {
        Some(d) => registry_caps(d)?,
        None => args.baseline.clone().context("give --baseline-src or --baseline")?,
    };
    let cand_registry = match &args.candidate_src {
        Some(d) => registry_caps(d)?,
        None => args.candidate.clone().context("give --candidate-src or --candidate")?,
    };

    // The tag differs, so the fleet can swap them.
    let base_wasm = build_probe(&base_dir, &args.component, "baseline")?;
    let cand_wasm = build_probe(&cand_dir, &args.component, "candidate")?;

    let fleet = Fleet::start_with_platform("selfimprove", 1);
    let api = Api::new(fleet.platform_url())?;
    let mut dep_id: Option<String> = None;

    let r_base = deploy_and_read(
        &api,
        &fleet,
        id,
        &mut dep_id,
        host,
        base_wasm,
        "baseline",
        &base_registry,
    )?;
    let base_caps = caps_of(&r_base);
    println!("  baseline  · healthy={} · {}", r_base["healthy"], show(&base_caps));

    let r_cand = deploy_and_read(
        &api,
        &fleet,
        id,
        &mut dep_id,
        host,
        cand_wasm.clone(),
        "candidate",
        &cand_registry,
    )?;
    let cand_caps = caps_of(&r_cand);
    println!("  candidate · healthy={} · {}", r_cand["healthy"], show(&cand_caps));

    // No capability lower and one advanced; a bare count misses in-place gains.
    let healthy = r_cand["healthy"].as_bool().unwrap_or(false);
    let v = compare(&base_caps, &cand_caps);
    println!();
    if !v.improvements.is_empty() {
        println!("  advances:    {}", v.improvements.join(", "));
    }
    if !v.regressions.is_empty() {
        println!("  regressions: {}", v.regressions.join(", "));
    }

    let more_capable = v.regressions.is_empty() && !v.improvements.is_empty();
    println!();
    if healthy && more_capable {
        println!("  PROMOTE — healthy, no regression, and strictly more capable.");
        println!("  the fleet is left running the candidate.");
        Ok(())
    } else {
        // Not an improvement: it does not keep the slot it was deployed into to be judged.
        let reason = if !healthy {
            "the candidate reported unhealthy".to_string()
        } else if !v.regressions.is_empty() {
            format!("the candidate regressed: {}", v.regressions.join(", "))
        } else {
            "the candidate advanced no capability".to_string()
        };
        println!("  ROLL BACK — {reason}.");
        let base_again = build_probe(&base_dir, &args.component, "baseline")?;
        let restored = deploy_and_read(
            &api,
            &fleet,
            id,
            &mut dep_id,
            host,
            base_again,
            "baseline",
            &base_registry,
        )?;
        println!("  restored baseline over the lattice: {}", show(&caps_of(&restored)));
        bail!("candidate rejected: {reason}");
    }
}
