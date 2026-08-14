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
//! The two capability sets are given as `--baseline` / `--candidate` so a run is
//! reproducible; the authentic source of the candidate is a loop that edited the
//! component, and the two builds are genuinely different bytes either way.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-selfimprove", about = "Promote a component version only if it is healthy and more capable, judged over the lattice.")]
struct Args {
    /// The self-target component. Must export wasi:http and report
    /// `{healthy, capability_count, capabilities}` (see `version-probe`).
    #[arg(long, default_value = "version-probe")]
    component: String,
    /// The baseline's advertised capabilities (comma-separated).
    #[arg(long)]
    baseline: String,
    /// The candidate's advertised capabilities (comma-separated).
    #[arg(long)]
    candidate: String,
}

/// Build the probe with a tag and a capability list; hand back the bytes.
fn build_probe(component: &str, tag: &str, caps: &str) -> Result<Vec<u8>> {
    let components = repo_root().join("components");
    let out = Command::new("cargo")
        .current_dir(&components)
        .args(["build", "--release", "--target", "wasm32-wasip2", "-p", component])
        .env("COMP_VERSION_TAG", tag)
        .env("COMP_CAPS", caps)
        .output()
        .context("running cargo")?;
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

fn caps_of(report: &Value) -> Vec<String> {
    report["capabilities"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn main() -> Result<()> {
    let args = Args::parse();
    let id = "self";
    let host = "self.improver.test";

    println!("comp-selfimprove: judging a candidate of `{}` over the lattice\n", args.component);

    // Two genuinely different builds — different tags AND different capability sets.
    let base_wasm = build_probe(&args.component, "baseline", &args.baseline)?;
    let cand_wasm = build_probe(&args.component, "candidate", &args.candidate)?;
    if base_wasm == cand_wasm {
        bail!("baseline and candidate built to identical bytes — there is nothing to promote");
    }

    let fleet = Fleet::start_with_platform("selfimprove", 1);
    let api = Api::new(fleet.platform_url())?;
    let mut dep_id: Option<String> = None;

    // --- deploy the baseline and ask it what it is --------------------------
    let r_base = deploy_and_read(&api, &fleet, id, &mut dep_id, host, base_wasm, "baseline")?;
    let base_caps = caps_of(&r_base);
    println!(
        "  baseline  · healthy={} · {} capabilities {:?}",
        r_base["healthy"], base_caps.len(), base_caps
    );

    // --- deploy the candidate over the top and ask again --------------------
    let r_cand = deploy_and_read(&api, &fleet, id, &mut dep_id, host, cand_wasm.clone(), "candidate")?;
    let cand_caps = caps_of(&r_cand);
    println!(
        "  candidate · healthy={} · {} capabilities {:?}",
        r_cand["healthy"], cand_caps.len(), cand_caps
    );

    // --- the decision -------------------------------------------------------
    let healthy = r_cand["healthy"].as_bool().unwrap_or(false);
    let kept_all = base_caps.iter().all(|c| cand_caps.contains(c));
    let gained: Vec<&String> = cand_caps.iter().filter(|c| !base_caps.contains(c)).collect();
    let more_capable = kept_all && !gained.is_empty();

    println!();
    if healthy && more_capable {
        println!("  PROMOTE — the candidate is healthy and strictly more capable (gained {gained:?}).");
        println!("  the fleet is left running the candidate.");
        Ok(())
    } else {
        // Roll the baseline back onto the fleet: a candidate that is not an
        // improvement does not get to keep the slot it was deployed into to be
        // judged. (The platform separately refuses a candidate that removes an
        // EXPORT; this refuses one that fails to advance the capability set.)
        let reason = if !healthy {
            "the candidate reported unhealthy".to_string()
        } else if !kept_all {
            "the candidate dropped a capability the baseline had".to_string()
        } else {
            "the candidate added no capability".to_string()
        };
        println!("  ROLL BACK — {reason}.");
        let base_again = build_probe(&args.component, "baseline", &args.baseline)?;
        let restored = deploy_and_read(&api, &fleet, id, &mut dep_id, host, base_again, "baseline")?;
        println!("  restored baseline over the lattice: {} capabilities", caps_of(&restored).len());
        bail!("candidate rejected: {reason}");
    }
}
