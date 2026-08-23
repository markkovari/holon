//! The benchmark matrix: every dimension crossed, and load held rather than spiked.
//!
//! Every number this project has published so far moved one axis for fifteen or
//! twenty seconds. That is enough to catch a throughput difference and exactly wrong
//! for the questions now being asked — what an idle app costs, whether sharing helps
//! at scale, whether anything leaks. A 20-second spike cannot see drift, and a
//! one-axis run cannot see an interaction.
//!
//! So: a cell per combination, load held for `--seconds`, and memory sampled
//! throughout rather than read once at the end.
//!
//! ```
//! comp-matrix --apps 1,8,32 --seconds 120
//! comp-matrix --apps 32 --seconds 600 --only distinct   # one long cell
//! ```

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use comp_reconciler::fleet::{bin_path, Fleet};

#[derive(Parser)]
#[command(name = "comp-matrix", about = "Cross every dimension, hold the load")]
struct Args {
    /// App counts to try, e.g. `1,8,32`.
    #[arg(long, default_value = "1,8,32", value_delimiter = ',')]
    apps: Vec<usize>,

    /// How long to hold load in each cell. Short runs cannot see drift.
    #[arg(long, default_value = "120")]
    seconds: u64,

    /// Concurrent connections during the load phase.
    #[arg(long, default_value = "64")]
    conns: usize,

    /// `same` (every app on one digest) or `distinct` (a digest each). Both by default.
    #[arg(long)]
    only: Option<String>,

    /// Cross each cell with the on-demand allocator as well.
    ///
    /// Pooling is the production default since ADR-0054, so it is what a cell
    /// measures unless you ask for both. A benchmark whose default differs from
    /// the deployment's default measures a configuration nobody runs.
    #[arg(long)]
    with_pool: bool,

    /// Nodes per fleet.
    #[arg(long, default_value = "1")]
    nodes: u16,

    /// Print the memory curve per cell, not just the endpoints.
    #[arg(long)]
    trace: bool,

    /// Requests per second to OFFER, regardless of what comes back.
    ///
    /// Without this the run is closed-loop: `--conns` requests in flight, each
    /// worker waiting for its own response, so concurrency is pinned and mean
    /// latency is forced to `conns / rps` by Little's law. Every latency figure in
    /// ADR-0053 and 0054 is that identity to two decimals. An arrival rate is what
    /// makes latency a measurement instead of arithmetic (ADR-0057).
    #[arg(long)]
    rate: Option<u32>,

    /// Storage backend for the guests: `memory`, `sqlite`, `nats`.
    ///
    /// The benchmark component does a keyvalue read and a write per request, so
    /// on `nats` every request pays two JetStream round trips and the rps column
    /// is a measurement of the bus rather than of the runtime. Running both is
    /// how you find out which one you were looking at (ADR-0057).
    #[arg(long)]
    kv: Option<String>,

    /// Send load straight at a host, skipping the ingress.
    ///
    /// The comparison is the point: every request in every previous benchmark was
    /// proxied, and nobody had measured what that hop costs.
    #[arg(long)]
    direct: bool,
}

#[derive(Debug)]
struct Cell {
    apps: usize,
    digests: &'static str,
    pool: bool,
    idle_mib: f64,
    loaded_mib: f64,
    drift_mib: f64,
    per_app_mib: f64,
    rps: f64,
    p50_ms: f64,
    p99_ms: f64,
    p999_ms: f64,
    shed: u64,
    failed: u64,
    first_error: Option<String>,
    statuses: std::collections::BTreeMap<u16, u64>,
    first_refusal: Option<String>,
    refusal_window: Option<(f64, f64)>,
    shared: usize,
    loaded_from_disk: usize,
    compiled: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let root = comp_reconciler::fleet::repo_root();
    // Honour the hand-composed artifact, derive it otherwise — the same rule the
    // fleet and the suites follow, so no caller needs `just compose-gate` first.
    let legacy = root.join("components/target/gate_domain.composed.wasm");
    let wasm = match std::fs::read(&legacy) {
        Ok(bytes) => bytes,
        Err(_) => {
            let catalog =
                comp_reconciler::plug::Catalog::scan(&comp_reconciler::plug::default_dirs(&root));
            comp_reconciler::plug::compose("gate-domain", &catalog)
                .map_err(|e| anyhow::anyhow!("composing gate-domain: {e} — `just build` first"))?
        }
    };

    let modes: Vec<&'static str> = match args.only.as_deref() {
        Some("same") => vec!["same"],
        Some("distinct") => vec!["distinct"],
        None => vec!["same", "distinct"],
        Some(other) => anyhow::bail!("--only takes `same` or `distinct`, not {other:?}"),
    };
    let pools: Vec<bool> = if args.with_pool { vec![false, true] } else { vec![true] };

    let total = args.apps.len() * modes.len() * pools.len();
    eprintln!(
        "comp-matrix: {total} cell(s), {}s of load each — about {} minutes\n",
        args.seconds,
        (total as u64 * (args.seconds + 40)) / 60
    );

    let mut cells = Vec::new();
    for &apps in &args.apps {
        for &digests in &modes {
            for &pool in &pools {
                eprintln!("  running apps={apps} digests={digests} pool={pool} …");
                cells.push(run_cell(&root, &wasm, apps, digests, pool, &args)?);
            }
        }
    }
    report(&cells, &args);
    Ok(())
}

/// N specs, and N artifacts when each app should have its own digest.
///
/// A distinct digest is the SAME component with a custom section appended — a wasm
/// custom section is inert, so behaviour is identical and only the content address
/// differs. Using genuinely different components instead would confound the memory
/// question with "these do different work".
fn write_inputs(
    dir: &std::path::Path,
    wasm: &[u8],
    apps: usize,
    distinct: bool,
) -> Result<Vec<(String, std::path::PathBuf)>> {
    std::fs::create_dir_all(dir.join("specs"))?;
    std::fs::create_dir_all(dir.join("art"))?;
    let mut artifacts = Vec::new();
    for i in 0..apps {
        let component = if distinct { format!("gate{i}") } else { "gate".to_string() };
        std::fs::write(
            dir.join("specs").join(format!("app{i}.yaml")),
            format!(
                "version: comp/v1\napp: app{i}\ntenant: t{i}\nstrategy: fused\n\
                 components:\n  - id: {component}\n\
                 ingress:\n  host: app{i}.matrix.test\n  component: {component}\n"
            ),
        )?;
        if distinct || i == 0 {
            let path = dir.join("art").join(format!("{component}.wasm"));
            let mut bytes = wasm.to_vec();
            if distinct {
                bytes.extend_from_slice(&custom_section(&format!("comp-matrix-{i}")));
            }
            std::fs::write(&path, &bytes)?;
            artifacts.push((component, path));
        }
    }
    Ok(artifacts)
}

/// A wasm custom section: id 0, then a LEB128 length, then a named payload. Ignored
/// by every runtime, which is the point — it changes the digest and nothing else.
fn custom_section(name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    leb128(name.len() as u64, &mut body);
    body.extend_from_slice(name.as_bytes());
    let mut out = vec![0u8];
    leb128(body.len() as u64, &mut out);
    out.extend_from_slice(&body);
    out
}

fn leb128(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            return;
        }
    }
}

fn rss_mib(pid: u32) -> f64 {
    let out =
        std::process::Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().ok();
    out.and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn run_cell(
    _root: &std::path::Path,
    wasm: &[u8],
    apps: usize,
    digests: &'static str,
    pool: bool,
    args: &Args,
) -> Result<Cell> {
    let dir = tempfile::tempdir()?;
    let artifacts = write_inputs(dir.path(), wasm, apps, digests == "distinct")?;
    let arts: Vec<String> =
        artifacts.iter().map(|(id, p)| format!("{id}={}", p.display())).collect();

    let lattice = format!("mx{apps}{digests}{}", u8::from(pool));
    let fleet = Fleet::start_bench(
        &lattice,
        dir.path().join("specs").to_str().unwrap(),
        &arts,
        args.nodes,
        pool,
        args.kv.as_deref(),
    );

    // Placed, then settled: an RSS read while instances are still arriving measures
    // the arrival, not the resting cost.
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline && fleet.started_count() < apps {
        std::thread::sleep(Duration::from_millis(500));
    }
    std::thread::sleep(Duration::from_secs(5));
    let pid = fleet.host_pid(1).context("the host is not running")?;
    let idle_mib = rss_mib(pid);

    // --- sustained load, memory sampled throughout ---
    //
    // Spread across every app, not just the first: one hot app and thirty-one idle
    // ones is a different measurement from thirty-two in use.
    let hosts: Vec<String> = (0..apps).map(|a| format!("app{a}.matrix.test")).collect();
    // Wait for the DOOR, not just the host. The host reporting an instance
    // started says nothing about the ingress having refreshed its route table,
    // and it refreshes on a timer — so load beginning a moment too early meets an
    // ingress that correctly answers "no replica of that is placed". Measured:
    // 12 851 spurious 503s in one run and none in the next, from the same binary.
    // That is the harness racing the platform, and it read as shedding.
    if !args.direct {
        for h in &hosts {
            anyhow::ensure!(
                fleet.serves(h, Duration::from_secs(60)),
                "the ingress never served {h}"
            );
        }
    }
    let door = if args.direct {
        fleet.host_port(1).context("no host port to send load at")?
    } else {
        fleet.ingress_port
    };
    let load = fleet.open_load(&hosts, door, args.rate, args.conns);

    // Let it reach steady state before the drift window opens, or the ramp counts as
    // a leak.
    std::thread::sleep(Duration::from_secs(10));
    let settled = rss_mib(pid);
    let started = Instant::now();
    let mut peak = settled;
    // The SHAPE, not just the delta. Growth that flattens is an allocator settling;
    // growth that keeps climbing is a leak, and a single before/after reading cannot
    // tell them apart — which is the whole reason this run exists.
    let mut trace: Vec<(u64, f64)> = vec![(0, settled)];
    while started.elapsed() < Duration::from_secs(args.seconds.saturating_sub(10)) {
        std::thread::sleep(Duration::from_secs(5));
        let now = rss_mib(pid);
        peak = peak.max(now);
        trace.push((started.elapsed().as_secs(), now));
    }
    if args.trace {
        println!("\n  RSS while loaded, apps={apps} digests={digests} pool={pool}:");
        for (t, mib) in trace.iter().step_by(if trace.len() > 24 { 4 } else { 1 }) {
            let bar = "#".repeat(((mib - settled).max(0.0) * 2.0) as usize);
            println!("    {t:>4}s  {mib:>7.1} MiB  {bar}");
        }
        // Second half against first: if the growth is front-loaded the curve is
        // settling, if it is even the process is still accumulating.
        let mid = trace.len() / 2;
        let first = trace[mid].1 - trace[0].1;
        let second = trace[trace.len() - 1].1 - trace[mid].1;
        println!(
            "    first half +{first:.2} MiB, second half +{second:.2} MiB -> {}",
            if second <= first * 0.4 {
                "flattening (allocator settling)"
            } else if second >= first * 0.8 {
                "still climbing (a leak, not a warm-up)"
            } else {
                "slowing, but not flat"
            }
        );
    }
    let loaded_mib = rss_mib(pid);
    let report = load.stop();
    // When something refused, say what the FLEET was doing at the time. A refusal
    // count plus a status code still cannot distinguish "the platform declined
    // load" from "the platform briefly lost track of a healthy node".
    if !report.statuses.is_empty() {
        let log = fleet.node_log("n1");
        let beats = log.lines().filter(|l| l.contains("heartbeat")).count();
        let rec = fleet.reconciler_log();
        eprintln!(
            "    diagnosis: host heartbeat complaints={beats}, reconciler stop commands={}",
            rec.lines().filter(|l| l.contains("stop")).count()
        );
        for l in log.lines().filter(|l| l.contains("heartbeat")).take(3) {
            eprintln!("      host: {l}");
        }
        for l in rec.lines().filter(|l| l.contains("stop") || l.contains("unschedulable")).take(3) {
            eprintln!("      reconciler: {l}");
        }
    }
    let elapsed = args.seconds as f64;
    let (shared, loaded_from_disk, compiled) = fleet.module_arrivals(1);
    Ok(Cell {
        apps,
        digests,
        pool,
        idle_mib,
        loaded_mib,
        // Growth AFTER steady state. Under a constant arrival rate this should be
        // flat; anything else is the thing a 20-second spike cannot see.
        drift_mib: loaded_mib - settled,
        per_app_mib: (idle_mib - 12.0) / apps as f64,
        rps: report.rps(elapsed),
        p50_ms: report.pct(50.0),
        p99_ms: report.pct(99.0),
        p999_ms: report.pct(99.9),
        shed: report.shed,
        failed: report.failed,
        first_error: report.first_error.clone(),
        statuses: report.statuses.clone(),
        first_refusal: report.first_refusal.clone(),
        refusal_window: report.refusal_window,
        shared,
        loaded_from_disk,
        compiled,
    })
}

/// Which door the load went through, for the header.
fn door(args: &Args) -> &'static str {
    if args.direct {
        "a host directly"
    } else {
        "the ingress"
    }
}

fn report(cells: &[Cell], args: &Args) {
    println!(
        "\n=== matrix: {}s of load per cell, {} workers, {} ===\n",
        args.seconds,
        args.conns,
        match args.rate {
            Some(r) => format!("OPEN loop at {r} rps offered, through {}", door(args)),
            // Said out loud, because the latency columns below are then
            // `conns / rps` by construction and mean nothing on their own.
            None => format!(
                "CLOSED loop (latency == conns/rps by Little's law), through {}",
                door(args)
            ),
        }
    );
    println!(
        "  {:>5} {:>9} {:>5} │ {:>8} {:>9} {:>8} │ {:>9} {:>7} {:>7} {:>7} {:>7} {:>5} │ {:>6} {:>5} {:>4}",
        "apps", "digests", "pool", "idle MiB", "loaded", "per-app", "rps", "p50 ms", "p99 ms",
        "p99.9", "4xx/5xx", "err", "shared", "disk", "cc"
    );
    for c in cells {
        println!(
            "  {:>5} {:>9} {:>5} │ {:>8.1} {:>9.1} {:>8.2} │ {:>9.0} {:>7.2} {:>7.2} {:>7.2} {:>7} {:>5} │ {:>6} {:>5} {:>4}",
            c.apps,
            c.digests,
            if c.pool { "on" } else { "off" },
            c.idle_mib,
            c.loaded_mib,
            c.per_app_mib,
            c.rps,
            c.p50_ms,
            c.p99_ms,
            c.p999_ms,
            c.shed,
            c.failed,
            c.shared,
            c.loaded_from_disk,
            c.compiled
        );
    }

    println!("\n  drift after steady state (a leak shows up here, not in a spike):");
    for c in cells {
        let verdict = if c.drift_mib > 5.0 {
            "  <- GROWING"
        } else if c.drift_mib > 1.0 {
            "  <- watch"
        } else {
            ""
        };
        println!(
            "    apps={:<4} digests={:<9} pool={:<4} {:+.2} MiB over {}s{verdict}",
            c.apps,
            c.digests,
            if c.pool { "on" } else { "off" },
            c.drift_mib,
            args.seconds - 10
        );
    }

    for c in cells.iter().filter(|c| !c.statuses.is_empty()) {
        let by: Vec<String> = c.statuses.iter().map(|(k, v)| format!("{k}x{v}")).collect();
        println!("\n  non-2xx at apps={}: {}", c.apps, by.join(" "));
        if let Some(r) = &c.first_refusal {
            println!("    first said: {r}");
        }
        if let Some((a, b)) = c.refusal_window {
            println!("    between {a:.1}s and {b:.1}s into the run");
        }
    }
    for c in cells.iter().filter(|c| c.failed > 0) {
        if let Some(e) = &c.first_error {
            println!("\n  {} transport failures at apps={}: {e}", c.failed, c.apps);
        }
    }

    // The comparison the matrix exists for: same-digest against distinct at equal
    // app counts. One number per pair, rather than two runs to eyeball.
    println!("\n  what sharing a digest saves, at equal app count:");
    for c in cells.iter().filter(|c| c.digests == "same") {
        if let Some(d) =
            cells.iter().find(|o| o.digests == "distinct" && o.apps == c.apps && o.pool == c.pool)
        {
            let saved = d.idle_mib - c.idle_mib;
            println!(
                "    {:>3} apps: {:>6.1} MiB shared vs {:>6.1} distinct — {:+.1} MiB ({:.0}% less)",
                c.apps,
                c.idle_mib,
                d.idle_mib,
                -saved,
                100.0 * saved / d.idle_mib.max(1.0)
            );
        }
    }
    let _ = bin_path("comp-host");
}
