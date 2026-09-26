//! `cargo xtask e2e` — was the `e2e-<app>` recipes (just/e2e.just).
//!
//! These drive a real fleet — a host, a composed artifact — and assert what it
//! serves. Almost every one was the same three lines (compose, build comp-host,
//! `cargo test --release` in the example), so they are one rule here; the
//! handful that ran a script instead are named.
//!
//! (Named `e2e_cmd` rather than `e2e`: a module and a function share Rust's
//! item namespace, and this one exports a function called `e2e`.)

use anyhow::Result;
use std::path::PathBuf;
use std::process::Command;

use crate::build::build_components;
use crate::compose::{comp_host, comp_plug, compose_app, derive, resolve_app};
use crate::util::{run_cmd, RELEASE};

fn run_script(script: &str) -> Result<()> {
    let mut cmd = Command::new("bash");
    cmd.arg(script);
    run_cmd(&mut cmd, &format!("bash {script}"))
}

pub(crate) fn e2e(target: &str, args: &[String]) -> Result<()> {
    match target {
        // RealWorld conformance (docs/apps/CONDUIT.md rung 4): the official Hurl
        // suite against the composed app. Needs `hurl`.
        "conformance-conduit" => {
            build_components(false)?;
            compose_app(Some("conduit"), false)?;
            comp_host()?;
            run_script("examples/conduit/conformance/run.sh")
        }
        // Durability proof (docs/apps/SAGA.md rung 3): kill the host mid-saga,
        // restart, show it resumes. Needs NATS on :4222.
        "durable-saga" => {
            build_components(false)?;
            compose_app(Some("saga"), false)?;
            comp_host()?;
            run_script("examples/saga/durability.sh")
        }
        // A saga whose legs are real durable Golem workers. Needs the Golem
        // binary — `cargo xtask e2e golem` fetches it once.
        "saga-golem" => {
            build_components(false)?;
            compose_app(Some("saga"), false)?;
            comp_host()?;
            run_script("examples/saga/golem-legs.sh")
        }
        // gate as a real Golem agent: exact serialization under a burst.
        "gate-golem" => run_script("examples/gate/golem-run.sh"),
        // The golem-workflow provider's live e2e (docs/capabilities/GOLEM.md
        // rung 3): downloads Golem 1.5, deploys the demo agent, invokes it.
        "golem" => run_script("providers/golem-workflow/e2e.sh"),
        // The `bytes:codec` spec run against the ARTIFACT, over HTTP through
        // codec-probe, so whatever satisfies the contract is what is judged.
        "gate-codec" => {
            comp_host()?;
            comp_plug()?;
            let mut cmd = Command::new("cargo");
            cmd.args([
                "build",
                "--release",
                "--target",
                "wasm32-wasip2",
                "-p",
                "bytes-codec",
                "-p",
                "codec-probe",
            ])
            .current_dir("components");
            run_cmd(&mut cmd, "build bytes-codec + codec-probe")?;
            run_script("components/bytes-codec/gate.sh")
        }
        "binder-poly" => binder_poly(args),
        app => e2e_app(app, args),
    }
}

fn e2e_app(app: &str, args: &[String]) -> Result<()> {
    let dir = PathBuf::from(format!("examples/{app}"));
    let cargo_suite = dir.join("Cargo.toml").is_file();
    let browser_suite = dir.join("playwright.config.ts").is_file();
    if !cargo_suite && !browser_suite {
        anyhow::bail!(
            "{} has no e2e suite (no Cargo.toml, no playwright.config.ts). Scripted variants: \
             conformance-conduit, durable-saga, saga-golem, gate-golem, golem, gate-codec, binder-poly",
            dir.display()
        );
    }

    build_components(false)?;
    compose_app(Some(app), false)?;
    comp_host()?;

    if cargo_suite {
        // mesh's suite spawns its own flaky upstream from `target/release/flaky`,
        // and the old recipe built it first — any example with a binary gets the
        // same, so the test never finds it missing.
        if dir.join("src/bin").is_dir() || dir.join("src/main.rs").is_file() {
            let mut bins = Command::new("cargo");
            bins.args(["build", "--release", "--bins"]).current_dir(&dir);
            run_cmd(&mut bins, &format!("cargo build --release --bins ({app})"))?;
        }
        let mut test = Command::new("cargo");
        test.args(["test", "--release"]).args(args).current_dir(&dir);
        return run_cmd(&mut test, &format!("cargo test --release ({app})"));
    }

    // A browser suite, because what is asserted needs one (poll: one vote per
    // browser is a cookie rule; console: the page, not the body, shows the run).
    // Fails loudly when a prerequisite is missing rather than skipping.
    if app == "console" {
        let mut seed = Command::new("cargo");
        seed.args([
            "build",
            "--manifest-path",
            "reconciler/Cargo.toml",
            "--release",
            "--bin",
            "comp-trace-seed",
        ]);
        run_cmd(&mut seed, "build comp-trace-seed")?;
    }
    let mut ci = Command::new("npm");
    ci.arg("ci").current_dir(&dir);
    run_cmd(&mut ci, &format!("npm ci ({app})"))?;
    let mut install = Command::new("npx");
    install.args(["playwright", "install", "--with-deps", "chromium"]).current_dir(&dir);
    run_cmd(&mut install, "playwright install chromium")?;
    let mut test = Command::new("npx");
    test.args(["playwright", "test"]).args(args).current_dir(&dir);
    run_cmd(&mut test, &format!("playwright test ({app})"))
}

/// The binder's own e2e, run against a composition built in another LANGUAGE.
///
/// `portfolio:value` and `price:history` are re-derived under
/// `components/<capability>-<lang>`, built through tools/build-polyglot.sh, and
/// swapped in for the Rust build by filename. Nothing in
/// examples/binder/tests/binder.rs is edited: the same assertions judge
/// whichever artifact satisfies the contract, which is the claim.
///
/// The Rust build is put back afterwards even when the test fails — it is what
/// `cargo xtask build` produces and what everything else composes against.
fn binder_poly(args: &[String]) -> Result<()> {
    let [lang, cap] = args else {
        anyhow::bail!(
            "usage: cargo xtask e2e binder-poly <lang> <capability>  (e.g. go portfolio-value)"
        );
    };
    build_components(false)?;
    let mut poly = Command::new("./tools/build-polyglot.sh");
    poly.args([lang, cap]);
    run_cmd(&mut poly, &format!("build-polyglot {lang} {cap}"))?;

    let plug = comp_plug()?;
    let binder = resolve_app("binder");
    let snake = cap.replace('-', "_");
    let rust_build = format!("{RELEASE}/{snake}.wasm");
    let backup = format!("components/target/{snake}.rust.wasm");
    std::fs::copy(&rust_build, &backup)?;

    let result = (|| -> Result<()> {
        std::fs::copy(format!("components/target/{snake}_{lang}.wasm"), &rust_build)?;
        derive(&plug, &binder.root, &binder.artifact)?;
        comp_host()?;
        let mut test = Command::new("cargo");
        test.args(["test", "--release"]).current_dir("examples/binder");
        run_cmd(&mut test, &format!("cargo test --release (binder, {cap} in {lang})"))
    })();

    std::fs::copy(&backup, &rust_build)?;
    derive(&plug, &binder.root, &binder.artifact)?;
    println!("restored the Rust build of {cap}");
    result
}
