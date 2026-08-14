//! `comp-selfimprove` — the recursive gate.
//!
//! Everything else ships a change. This DECIDES whether to keep it, using the one
//! judge that cannot be faked: the running code's own report, read over the
//! lattice. It builds a candidate version of a component, deploys it over the
//! running baseline on a live fleet, asks it over NATS whether it is healthy and
//! what it can do, and PROMOTES it only if it is healthy and strictly more
//! capable than the baseline. Otherwise it rolls the baseline back.
//!
//! The platform already refuses an upgrade that removes an EXPORT (a capability at
//! the interface). This is the tier above that: a capability at the semantic
//! level — advertised and self-reported — may grow and may not silently shrink.
//!
//! Two ways to say what to judge. `--baseline-src`/`--candidate-src` build the
//! REAL component from two source trees — the honest form: the bytes that deploy
//! carry the capabilities the loop actually wrote into the manifest. Or
//! `--baseline`/`--candidate` hand in capability lists (via `COMP_CAPS`) for a
//! reproducible run without two trees. Either way the two builds are different
//! bytes and the judge is the running code's own report.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use comp_reconciler::fleet::{repo_root, Fleet};
use semver::Version;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-selfimprove", about = "Promote a component version only if it is healthy and more capable, judged over the lattice.")]
struct Args {
    /// The self-target component. Must export wasi:http and report
    /// `{healthy, capability_count, capabilities}` (see `version-probe`).
    #[arg(long, default_value = "version-probe")]
    component: String,
    /// The baseline's advertised capabilities (comma-separated). Ignored when
    /// `--baseline-src` is given.
    #[arg(long)]
    baseline: Option<String>,
    /// The candidate's advertised capabilities (comma-separated). Ignored when
    /// `--candidate-src` is given.
    #[arg(long)]
    candidate: Option<String>,
    /// A source tree to build the baseline component FROM — its own manifest is
    /// what the deployed bytes report. The honest form.
    #[arg(long)]
    baseline_src: Option<PathBuf>,
    /// A source tree to build the candidate component FROM.
    #[arg(long)]
    candidate_src: Option<PathBuf>,
}

/// Build the probe with a tag, in the given `components` dir. `caps` overrides
/// the manifest via `COMP_CAPS` when set; when `None`, the component reports the
/// tree's own baked-in manifest — the bytes are the loop's real output.
fn build_probe(components: &Path, component: &str, tag: &str, caps: Option<&str>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(components)
        .args(["build", "--release", "--target", "wasm32-wasip2", "-p", component])
        .env("COMP_VERSION_TAG", tag);
    match caps {
        Some(c) => {
            cmd.env("COMP_CAPS", c);
        }
        // Clear any COMP_CAPS the parent set, so the manifest is what is baked in.
        None => {
            cmd.env_remove("COMP_CAPS");
        }
    }
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
        let code = self
            .http
            .post(format!("{}/api/components?id={id}", self.base))
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
        match self.http.post(format!("{}{path}", self.base)).bearer_auth(&self.token).json(&body).send() {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or(Value::Null)),
            Err(e) => (0, Value::String(format!("transport: {e}"))),
        }
    }
}

/// The whole self-report the running version answers with, over the lattice.
fn report(fleet: &Fleet, host: &str) -> Option<Value> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().ok()?;
    let r = http
        .get(format!("http://127.0.0.1:{}/", fleet.ingress_port))
        .header("host", host)
        .send()
        .ok()?;
    serde_json::from_str(&r.text().ok()?).ok()
}

/// Deploy `wasm` under `id` (creating the deployment on the first call) and wait
/// until the version tagged `want` answers over the lattice. Returns its report.
fn deploy_and_read(
    api: &Api,
    fleet: &Fleet,
    id: &str,
    dep_id: &mut Option<String>,
    host: &str,
    wasm: Vec<u8>,
    want: &str,
) -> Result<Value> {
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
        // Save is retried: an upload clears the digest, and the reconciler's push
        // pass must stage the bytes before a revision can name them (ADR-0006).
        let _ = api.post(&format!("/api/deployments/{did}/save"), json!({}));
        if report(fleet, host).and_then(|r| r["tag"].as_str().map(str::to_string)).as_deref() == Some(want) {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    report(fleet, host).with_context(|| format!("{want} never answered over the lattice"))
}

/// The capability→semver map the running version reports over the lattice.
fn caps_of(report: &Value) -> BTreeMap<String, String> {
    report["capabilities"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect()
}

/// Parse a capability version. An unparseable version sorts as `0.0.0` — the
/// lowest — so a candidate that ships a malformed version looks like a downgrade
/// and is refused, which is the safe direction.
fn ver(s: &str) -> Version {
    Version::parse(s.trim()).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// Render a map as `name@semver` for a human.
fn show(caps: &BTreeMap<String, String>) -> String {
    let parts: Vec<String> = caps.iter().map(|(n, v)| format!("{n}@{v}")).collect();
    format!("[{}]", parts.join(", "))
}

/// The verdict of comparing two capability maps.
struct Verdict {
    /// A baseline capability the candidate dropped or downgraded — a regression.
    regressions: Vec<String>,
    /// A capability the candidate added or raised the semver of.
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
    let id = "self";
    let host = "self.improver.test";

    println!("comp-selfimprove: judging a candidate of `{}` over the lattice\n", args.component);

    // Where to build each side from, and how. A source tree wins over a caps
    // string: the honest form builds the loop's real bytes from its manifest.
    let here = repo_root().join("components");
    let base_dir = args.baseline_src.clone().map(|d| d.join("components")).unwrap_or_else(|| here.clone());
    let cand_dir = args.candidate_src.clone().map(|d| d.join("components")).unwrap_or_else(|| here.clone());
    let base_caps_opt = if args.baseline_src.is_some() { None } else { args.baseline.as_deref() };
    let cand_caps_opt = if args.candidate_src.is_some() { None } else { args.candidate.as_deref() };
    if base_caps_opt.is_none() && args.baseline_src.is_none() {
        bail!("give --baseline-src or --baseline");
    }
    if cand_caps_opt.is_none() && args.candidate_src.is_none() {
        bail!("give --candidate-src or --candidate");
    }

    // Two genuinely different builds — different tags AND different manifests/caps.
    let base_wasm = build_probe(&base_dir, &args.component, "baseline", base_caps_opt)?;
    let cand_wasm = build_probe(&cand_dir, &args.component, "candidate", cand_caps_opt)?;
    if base_wasm == cand_wasm {
        bail!("baseline and candidate built to identical bytes — there is nothing to promote");
    }

    let fleet = Fleet::start_with_platform("selfimprove", 1);
    let api = Api::new(fleet.platform_url())?;
    let mut dep_id: Option<String> = None;

    // --- deploy the baseline and ask it what it is --------------------------
    let r_base = deploy_and_read(&api, &fleet, id, &mut dep_id, host, base_wasm, "baseline")?;
    let base_caps = caps_of(&r_base);
    println!("  baseline  · healthy={} · {}", r_base["healthy"], show(&base_caps));

    // --- deploy the candidate over the top and ask again --------------------
    let r_cand = deploy_and_read(&api, &fleet, id, &mut dep_id, host, cand_wasm.clone(), "candidate")?;
    let cand_caps = caps_of(&r_cand);
    println!("  candidate · healthy={} · {}", r_cand["healthy"], show(&cand_caps));

    // --- the decision -------------------------------------------------------
    // More capable = it kept every capability at no LOWER a version (no
    // regression) AND advanced at least one — a new capability, or a higher
    // version of one it already had. The version map is what makes the second
    // half visible: a bare count cannot see `diff-writer` improve in place.
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
        // Roll the baseline back onto the fleet: a candidate that is not an
        // improvement does not get to keep the slot it was deployed into to be
        // judged. (The platform separately refuses a candidate that removes an
        // EXPORT; this refuses one that fails to advance the capability map.)
        let reason = if !healthy {
            "the candidate reported unhealthy".to_string()
        } else if !v.regressions.is_empty() {
            format!("the candidate regressed: {}", v.regressions.join(", "))
        } else {
            "the candidate advanced no capability".to_string()
        };
        println!("  ROLL BACK — {reason}.");
        let base_again = build_probe(&base_dir, &args.component, "baseline", base_caps_opt)?;
        let restored = deploy_and_read(&api, &fleet, id, &mut dep_id, host, base_again, "baseline")?;
        println!("  restored baseline over the lattice: {}", show(&caps_of(&restored)));
        bail!("candidate rejected: {reason}");
    }
}
