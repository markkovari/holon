//! The reading half of the benchmarks: parse a result, print one honest line.
//!
//! These were eight small Python scripts that each grew their own idea of what
//! "failed" means. Two of them silently reported nothing for the exact interval being
//! measured, and one counted every 5xx as a shed when most were the ingress saying it
//! had no route at all — mistakes that survived because each script was the only
//! reader of its own output.
//!
//! One binary, one set of definitions. The scripts still orchestrate processes and
//! ssh, which is what shell is good at; nothing interprets a number in bash.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "comp-bench", about = "Read benchmark output and say what it means")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// One line from an `oha --output-format json` result.
    Summarise {
        path: std::path::PathBuf,
        #[arg(default_value = "")]
        label: String,
    },
    /// Median phase costs from a host's own `started … in N us (…)` lines.
    Coldstart { log: std::path::PathBuf },
    /// Total replicas the fleet is running, from an inventory dump on stdin.
    Replicas,
    /// Open-loop load: a fixed arrival RATE, bucketed per second, across whatever
    /// happens to the fleet while it runs.
    Load {
        /// Where to send it, e.g. http://127.0.0.1:8090/api/ratelimit
        url: String,
        #[arg(long, default_value = "shop.eve.test")]
        host: String,
        #[arg(long, default_value = "60")]
        secs: u64,
        /// Requests per second. This is an ARRIVAL rate: it does not fall when the
        /// fleet slows down, which is the whole point (ADR-0036).
        #[arg(long, default_value = "1000")]
        rate: u64,
        #[arg(long, default_value = "64")]
        workers: usize,
        /// A file of `<unix-seconds> <what happened>` lines to mark on the output.
        #[arg(long)]
        events: Option<std::path::PathBuf>,
    },
    /// One percentile line for a rate-limit budget, from an inventory sample file.
    Converge {
        /// `<unix-seconds> <replica-count>` per line.
        samples: std::path::PathBuf,
        #[arg(long)]
        events: Option<std::path::PathBuf>,
        /// The count the fleet is supposed to hold.
        #[arg(long, default_value = "5")]
        want: u64,
    },
    /// Which ORGANISATIONS each node holds, and whether any node holds more than
    /// one. The assertion behind ADR-0034: tenants must not be mapped to machines.
    Tenants {
        #[arg(long, default_value = "nats://127.0.0.1:4232")]
        nats_url: String,
        #[arg(long, default_value = "default")]
        lattice: String,
    },
    /// Which node holds what, read straight from the lattice.
    Inventory {
        #[arg(long, default_value = "nats://127.0.0.1:4232")]
        nats_url: String,
        #[arg(long, default_value = "default")]
        lattice: String,
    },
}

fn main() -> Result<()> {
    match Args::parse().cmd {
        Cmd::Summarise { path, label } => summarise(&path, &label),
        Cmd::Coldstart { log } => coldstart(&log),
        Cmd::Replicas => replicas(),
        Cmd::Inventory { nats_url, lattice } => inventory(&nats_url, &lattice),
        Cmd::Tenants { nats_url, lattice } => tenants(&nats_url, &lattice),
        Cmd::Load { url, host, secs, rate, workers, events } => {
            load(&url, &host, secs, rate, workers, events.as_deref())
        }
        Cmd::Converge { samples, events, want } => converge(&samples, events.as_deref(), want),
    }
}

/// The fleet as the reconciler sees it.
///
/// Read through the lattice crate rather than by shelling out to `nats kv get`, which
/// also retires a footgun that cost two runs: `--raw` writes no trailing newline, so
/// several nodes concatenated into one invalid line and the sampler reported nothing
/// for exactly the window being measured.
fn inventory(nats_url: &str, lattice: &str) -> Result<()> {
    use comp_lattice::nats::NatsLattice;
    use comp_lattice::Inventory as _;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let l = NatsLattice::connect(nats_url, lattice, std::time::Duration::from_secs(15)).await?;
        let mut total = 0u64;
        let mut rows: Vec<String> = Vec::new();
        for e in l.read_all().await? {
            let Ok(v) = serde_json::from_slice::<Value>(&e.value) else { continue };
            let mut held: Vec<String> = Vec::new();
            if let Some(items) = v["instances"].as_array() {
                for i in items {
                    let n = i["count"].as_u64().unwrap_or(0);
                    total += n;
                    held.push(format!(
                        "{}/{} x{n}",
                        i["tenant"].as_str().unwrap_or("?"),
                        i["app"].as_str().unwrap_or("?")
                    ));
                }
            }
            rows.push(format!(
                "    {:10} ({:>2} cpu) {}",
                v["node"].as_str().unwrap_or("?"),
                v["capacity"]["cpus"].as_u64().unwrap_or(0),
                if held.is_empty() { "-".to_string() } else { held.join(", ") }
            ));
        }
        rows.sort();
        for r in rows {
            println!("{r}");
        }
        println!("    total {total} replica(s)");
        Ok::<_, anyhow::Error>(())
    })
}

fn summarise(path: &std::path::Path, label: &str) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let d: Value = serde_json::from_str(&text).with_context(|| format!("{}", path.display()))?;
    let s = &d["summary"];
    let p = &d["latencyPercentiles"];
    let codes: BTreeMap<String, u64> = serde_json::from_value(
        d["statusCodeDistribution"].clone(),
    )
    .unwrap_or_default();
    let errs: BTreeMap<String, u64> =
        serde_json::from_value(d["errorDistribution"].clone()).unwrap_or_default();

    let ok: u64 = codes.iter().filter(|(k, _)| k.starts_with('2')).map(|(_, v)| v).sum();
    let non2xx: u64 = codes.iter().filter(|(k, _)| !k.starts_with('2')).map(|(_, v)| v).sum();
    // oha counts requests still in flight when the clock stops as errors. With 200
    // connections that is 200 every run, and calling it "failed" overstates every
    // result by exactly the connection count.
    let aborted: u64 = errs.iter().filter(|(k, _)| k.contains("deadline")).map(|(_, v)| v).sum();
    let transport: u64 = errs.values().sum::<u64>() - aborted;

    println!(
        "  {label:14} {:8.0} rps   p50 {:7.1} ms   p99 {:8.1} ms   {ok} ok / {non2xx} non-2xx / {transport} failed / {aborted} in flight at end",
        s["requestsPerSec"].as_f64().unwrap_or(0.0),
        1000.0 * p["p50"].as_f64().unwrap_or(0.0),
        1000.0 * p["p99"].as_f64().unwrap_or(0.0),
    );
    if non2xx > 0 && ok == 0 {
        // The failure that produced ADR-0036's correction: 102k rps that were all
        // ingress 503s, reported as "100% success" because oha counts completions.
        println!("  {:14} NO 2xx AT ALL — this measured an error path: {codes:?}", "");
    }
    Ok(())
}

fn median(mut v: Vec<u64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable();
    v[v.len() / 2] as f64
}

fn coldstart(log: &std::path::Path) -> Result<()> {
    let text = std::fs::read_to_string(log).with_context(|| format!("{}", log.display()))?;
    let (mut cold, mut warm) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let Some(rest) = line.split(" in ").nth(1) else { continue };
        let Some((total, tail)) = rest.split_once(" us (") else { continue };
        let Ok(total) = total.trim().parse::<u64>() else { continue };
        let nums: Vec<u64> = tail
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        if nums.len() < 3 {
            continue;
        }
        let row = (total, nums[0], nums[1], nums[2]);
        if tail.contains("cache-load") { warm.push(row) } else { cold.push(row) }
    }
    for (rows, what) in [(&cold, "cold: wasmtime compiles"), (&warm, "warm: loaded from cache")] {
        if rows.is_empty() {
            continue;
        }
        println!("=== {what}: {} start(s) ===", rows.len());
        for (i, name) in ["total", "fetch", "build", "link"].iter().enumerate() {
            let vals: Vec<u64> = rows
                .iter()
                .map(|r| match i {
                    0 => r.0,
                    1 => r.1,
                    2 => r.2,
                    _ => r.3,
                })
                .collect();
            println!(
                "  {name:8} median {:8.2} ms   min {:7.2} ms   max {:7.2} ms",
                median(vals.clone()) / 1000.0,
                *vals.iter().min().unwrap() as f64 / 1000.0,
                *vals.iter().max().unwrap() as f64 / 1000.0
            );
        }
    }
    if !cold.is_empty() && !warm.is_empty() {
        let (a, b) = (median(cold.iter().map(|r| r.0).collect()),
                      median(warm.iter().map(|r| r.0).collect()));
        println!(
            "\n  {:.1} ms -> {:.2} ms, a {:.0}x cut ({:.1}% of the start removed).",
            a / 1000.0,
            b / 1000.0,
            a / b.max(1.0),
            100.0 * (a - b) / a.max(1.0)
        );
    }
    Ok(())
}

fn replicas() -> Result<()> {
    // One JSON inventory entry per line, as `nats kv get … --raw` produces once each
    // is newline-terminated. `--raw` writes NO trailing newline, which silently
    // concatenated several nodes into one invalid line and reported zero for exactly
    // the interval being measured — hence reading line by line and skipping junk
    // rather than parsing the whole stream as one document.
    let mut total = 0u64;
    for line in std::io::stdin().lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        total += v["instances"]
            .as_array()
            .map(|a| a.iter().filter_map(|i| i["count"].as_u64()).sum::<u64>())
            .unwrap_or(0);
    }
    println!("{total}");
    Ok(())
}


/// `<unix-seconds> <label>` per line: what happened, and when.
fn read_events(path: Option<&std::path::Path>) -> Vec<(u64, String)> {
    let Some(path) = path else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut out: Vec<(u64, String)> = text
        .lines()
        .filter_map(|l| {
            let (at, what) = l.trim().split_once(' ')?;
            Some((at.parse::<f64>().ok()? as u64, what.to_string()))
        })
        .collect();
    out.sort();
    out
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Open-loop load, bucketed per second.
///
/// Open-loop matters: a closed-loop generator waits for replies, so when nodes die
/// the offered load falls by itself and the survivors never get the dead nodes'
/// share. ADR-0036 measured the difference — the same fleet answered a p99 of 46
/// SECONDS with zero errors, which a closed loop cannot produce.
///
/// A request that outlives the timeout counts as a failure: from a caller's side an
/// eight second wait IS an outage, whatever it eventually returns.
fn load(
    url: &str,
    host: &str,
    secs: u64,
    rate: u64,
    workers: usize,
    events: Option<&std::path::Path>,
) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let started = std::time::Instant::now();
    let wall = now();
    let buckets: Arc<Vec<(AtomicU64, AtomicU64)>> =
        Arc::new((0..secs + 1).map(|_| (AtomicU64::new(0), AtomicU64::new(0))).collect());

    let per_worker = std::time::Duration::from_nanos(
        1_000_000_000u64.saturating_mul(workers as u64) / rate.max(1),
    );
    let mut handles = Vec::new();
    for _ in 0..workers {
        let (buckets, url, host) = (buckets.clone(), url.to_string(), host.to_string());
        handles.push(std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap();
            let mut next = std::time::Instant::now();
            while started.elapsed().as_secs() < secs {
                next += per_worker;
                let sleep = next.saturating_duration_since(std::time::Instant::now());
                if !sleep.is_zero() {
                    std::thread::sleep(sleep);
                }
                let at = started.elapsed().as_secs().min(secs) as usize;
                let ok = client
                    .post(&url)
                    .header("host", &host)
                    .json(&serde_json::json!({
                        "key": "load", "capacity": 100_000_000u64, "refill": 100_000_000u64
                    }))
                    .send()
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                let b = &buckets[at];
                if ok { b.0.fetch_add(1, Ordering::Relaxed) } else { b.1.fetch_add(1, Ordering::Relaxed) };
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }

    let marks: BTreeMap<u64, String> = read_events(events)
        .into_iter()
        .map(|(at, what)| (at.saturating_sub(wall), what))
        .collect();
    println!("    sec   ok   fail");
    let (mut total_ok, mut total_bad) = (0u64, 0u64);
    for (i, b) in buckets.iter().enumerate() {
        let (ok, bad) = (b.0.load(Ordering::Relaxed), b.1.load(Ordering::Relaxed));
        total_ok += ok;
        total_bad += bad;
        let mark = marks.get(&(i as u64)).map(|m| format!("   <- {m}")).unwrap_or_default();
        // Only the interesting seconds: any failure, any event, or the edges.
        if bad > 0 || !mark.is_empty() || i < 2 || i as u64 == secs {
            println!("    {i:4} {ok:5} {bad:6}{mark}");
        }
    }
    let pct = 100.0 * total_bad as f64 / (total_ok + total_bad).max(1) as f64;
    println!("\n    {total_ok} ok, {total_bad} failed ({pct:.2}%)");

    // Each window is bounded by the NEXT event, or one kill gets blamed for the
    // other's errors — which it was, and it read as a 59s recovery that never
    // happened.
    let ev: Vec<(u64, String)> = marks.into_iter().collect();
    for (i, (at, what)) in ev.iter().enumerate() {
        let end = ev.get(i + 1).map(|(t, _)| *t).unwrap_or(secs);
        let bad: Vec<u64> = (*at..end)
            .filter(|s| buckets[*s as usize].1.load(Ordering::Relaxed) > 0)
            .collect();
        match (bad.first(), bad.last()) {
            (Some(f), Some(l)) => println!(
                "    after {what:?}: errors in seconds {f}..{l} ({}s from the event to the last error)",
                l - at + 1
            ),
            _ => println!("    after {what:?}: no failed request at all"),
        }
    }
    Ok(())
}

/// How long the fleet ran under-replicated, per event.
fn converge(samples: &std::path::Path, events: Option<&std::path::Path>, want: u64) -> Result<()> {
    let text = std::fs::read_to_string(samples)
        .with_context(|| format!("reading {}", samples.display()))?;
    let rows: Vec<(u64, u64)> = text
        .lines()
        .filter_map(|l| {
            let (t, n) = l.trim().split_once(' ')?;
            Some((t.parse().ok()?, n.trim().parse().ok()?))
        })
        .collect();
    if rows.is_empty() {
        println!("    no samples");
        return Ok(());
    }
    let ev = read_events(events);
    for (i, (at, what)) in ev.iter().enumerate() {
        let end = ev.get(i + 1).map(|(t, _)| *t).unwrap_or(u64::MAX);
        let after: Vec<(u64, u64)> =
            rows.iter().copied().filter(|(t, _)| *t >= *at && *t < end).collect();
        // The total does NOT drop the moment a node is killed — its inventory entry
        // lives out the TTL first (ADR-0022). So find the DIP, then the recovery;
        // latching onto the pre-dip value reported a 1s recovery that was really the
        // reading taken before anything had been noticed.
        let Some((dip_at, _)) = after.iter().copied().find(|(_, n)| *n < want) else {
            println!("    {what}: never observed below {want}");
            continue;
        };
        let low = after.iter().map(|(_, n)| *n).min().unwrap_or(want);
        match after.iter().copied().find(|(t, n)| *t > dip_at && *n >= want) {
            Some((back, _)) => println!(
                "    {what}: noticed {}s after, low water {low}/{want}, back to {want} {}s after ({}s to re-place)",
                dip_at - at,
                back - at,
                back - dip_at
            ),
            None => println!(
                "    {what}: noticed {}s after, low water {low}/{want}, NOT restored before the run ended",
                dip_at - at
            ),
        }
    }
    Ok(())
}


/// Organisations per node, and how many nodes hold more than one.
///
/// Read from inventory rather than by scraping every host's log — which is what this
/// replaced, and which needed an ssh to each remote machine to collect logs that the
/// lattice was already publishing. The bar is not "it runs on several machines": giving
/// each org its own box would pass that while quietly turning a multi-tenant platform
/// into two single-tenant ones with a shared login page (ADR-0034).
fn tenants(nats_url: &str, lattice: &str) -> Result<()> {
    use comp_lattice::nats::NatsLattice;
    use comp_lattice::Inventory as _;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let l = NatsLattice::connect(nats_url, lattice, std::time::Duration::from_secs(15)).await?;
        let mut by_node: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for e in l.read_all().await? {
            let Ok(v) = serde_json::from_slice::<Value>(&e.value) else { continue };
            let node = v["node"].as_str().unwrap_or("?").to_string();
            let set = by_node.entry(node).or_default();
            if let Some(items) = v["instances"].as_array() {
                for i in items {
                    if let Some(t) = i["tenant"].as_str() {
                        set.insert(t.to_string());
                    }
                }
            }
        }
        // A node holding nothing is not evidence either way, so it is shown but not
        // counted — otherwise an idle node drags the ratio down and reads as a failure.
        let holding: Vec<_> = by_node.iter().filter(|(_, t)| !t.is_empty()).collect();
        let mixed = holding.iter().filter(|(_, t)| t.len() > 1).count();
        for (node, orgs) in &by_node {
            let list = if orgs.is_empty() {
                "-".to_string()
            } else {
                orgs.iter().cloned().collect::<Vec<_>>().join(" + ")
            };
            let tag = if orgs.len() > 1 { "  <- both orgs" } else { "" };
            println!("    {node:10} {list}{tag}");
        }
        println!(
            "\n  {mixed}/{} node(s) holding work hold MORE THAN ONE organisation — {}",
            holding.len(),
            if mixed > 0 {
                "tenants are not mapped to machines"
            } else {
                "WARNING: every node holds a single org"
            }
        );
        Ok::<_, anyhow::Error>(())
    })
}
