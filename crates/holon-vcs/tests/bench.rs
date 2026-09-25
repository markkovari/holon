//! Graph lookup latency on SurrealDB with a realistically large workspace.
//! Env-gated twice — it needs a server AND takes a while:
//!
//!   HOLON_VCS_BENCH=1 HOLON_VCS_SURREAL_URL=127.0.0.1:8000 \
//!   cargo test --manifest-path crates/Cargo.toml --features holon-vcs/native \
//!     --release --test bench -- --nocapture
//!
//! Inserts 5 000 symbols and 20 000 patches (each with a `depends_on` edge) and
//! 1 000 conflicts into one workspace of a fresh database, timing the writes as
//! the tables grow, then times every lookup the engine makes. Asserts each
//! stays bounded: an unindexed lookup is a table scan, and grows with the data.
#![cfg(feature = "native")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use holon_vcs::graph::{Change, ConflictRecord, Graph, PatchRecord, PatchStatus, SideRecord};
use holon_vcs::model::{Agent, ConflictState, SymbolId, SymbolKind};
use holon_vcs::store::sha256_hex;
use holon_vcs::surreal::{SurrealConfig, SurrealGraph};

const SYMBOLS: usize = 5_000;
const PATCHES_PER_SYMBOL: usize = 4;
const CONFLICTS: usize = 1_000;
const COMPONENTS: usize = 100;
const CONCURRENCY: usize = 32;

fn sym(i: usize) -> SymbolId {
    SymbolId::new(
        &format!("c{}", i % COMPONENTS),
        &format!("src/f{}.rs", i % 7),
        &format!("f{i}"),
        SymbolKind::Function,
    )
}

fn key(i: usize) -> String {
    sha256_hex(format!("key{i}").as_bytes())
}

fn phash(i: usize, j: usize) -> String {
    sha256_hex(format!("patch{i}/{j}").as_bytes())
}

fn patch(i: usize, j: usize) -> PatchRecord {
    PatchRecord {
        hash: phash(i, j),
        key: key(i),
        symbol: sym(i),
        parents: if j == 0 { vec![] } else { vec![phash(i, j - 1)] },
        change: Change::Replace(sha256_hex(format!("content{i}/{j}").as_bytes())),
        content: Some(sha256_hex(format!("content{i}/{j}").as_bytes())),
        agent: Agent::named("bench"),
        message: None,
        at: 0,
        op: Some((i * PATCHES_PER_SYMBOL + j) as u64 + 1),
        status: PatchStatus::Landed,
        depends_on: vec![sym((i + 1) % SYMBOLS)],
        implements: vec![],
        wit_binding: None,
        status_op: 1,
        order: None,
        placement: None,
    }
}

fn conflict(i: usize) -> ConflictRecord {
    ConflictRecord {
        id: sha256_hex(format!("conflict{i}").as_bytes()),
        key: key(i),
        symbol: sym(i),
        base: Some(phash(i, 0)),
        left: SideRecord { patch: phash(i, 1), agent: Agent::named("l"), content: None },
        right: SideRecord { patch: phash(i, 2), agent: Agent::named("r"), content: None },
        state: if i.is_multiple_of(2) { ConflictState::Open } else { ConflictState::Resolved },
        opened_at: 0,
        resolved_by: None,
        pending: vec![],
        state_op: 0,
    }
}

struct Stats {
    name: &'static str,
    samples: Vec<Duration>,
}

impl Stats {
    fn new(name: &'static str) -> Self {
        Stats { name, samples: vec![] }
    }
    fn pct(&self, p: f64) -> Duration {
        let mut s = self.samples.clone();
        s.sort();
        s[((s.len() as f64 - 1.0) * p).round() as usize]
    }
    fn report(&self) -> Duration {
        let p99 = self.pct(0.99);
        eprintln!(
            "  {:<28} n={:<5} p50={:>8.2?} p99={:>8.2?} max={:>8.2?}",
            self.name,
            self.samples.len(),
            self.pct(0.5),
            p99,
            self.pct(1.0)
        );
        p99
    }
}

async fn time<F: std::future::Future>(stats: &mut Stats, f: F) -> F::Output {
    let t = Instant::now();
    let out = f.await;
    stats.samples.push(t.elapsed());
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn graph_lookups_stay_bounded() {
    let (Ok(_), Ok(url)) =
        (std::env::var("HOLON_VCS_BENCH"), std::env::var("HOLON_VCS_SURREAL_URL"))
    else {
        eprintln!("SKIPPED bench: set HOLON_VCS_BENCH=1 and HOLON_VCS_SURREAL_URL to run it");
        return;
    };
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let mut cfg = SurrealConfig::new(&url);
    cfg.namespace = "holon_vcs_bench".into();
    cfg.database = format!("bench_{}_{nanos}", std::process::id());
    let g = Arc::new(SurrealGraph::connect(&cfg).await.expect("surreal"));
    let ws = "bench";

    // ---- inserts, timed per tenth of the data --------------------------------
    eprintln!(
        "inserting {SYMBOLS} symbols, {} patches, {CONFLICTS} conflicts",
        SYMBOLS * PATCHES_PER_SYMBOL
    );
    let started = Instant::now();
    let mut first_tenth = Stats::new("put_patch, first 10%");
    let mut last_tenth = Stats::new("put_patch, last 10%");
    let chunk = SYMBOLS / 10;
    for tenth in 0..10 {
        let mut tasks = Vec::new();
        for w in 0..CONCURRENCY {
            let g = g.clone();
            tasks.push(tokio::spawn(async move {
                let mut lat = Vec::new();
                let mut i = tenth * chunk + w;
                while i < (tenth + 1) * chunk {
                    g.ensure_symbol(ws, &key(i), &sym(i)).await.unwrap();
                    for j in 0..PATCHES_PER_SYMBOL {
                        let t = Instant::now();
                        g.put_patch(ws, &patch(i, j)).await.unwrap();
                        lat.push(t.elapsed());
                    }
                    i += CONCURRENCY;
                }
                lat
            }));
        }
        for t in tasks {
            let lat = t.await.unwrap();
            match tenth {
                0 => first_tenth.samples.extend(lat),
                9 => last_tenth.samples.extend(lat),
                _ => {}
            }
        }
    }
    let mut conflicts_put = Stats::new("put_conflict");
    for i in 0..CONFLICTS {
        let i = i * (SYMBOLS / CONFLICTS);
        let c = conflict(i);
        time(&mut conflicts_put, g.put_conflict(ws, &c, 1)).await.unwrap();
        let effect = holon_vcs::graph::ConflictEffect {
            conflict: c.id.clone(),
            before: ConflictState::Abandoned,
            after: c.state,
            resolved_by_before: None,
            resolved_by_after: None,
        };
        g.apply_conflict_effect(ws, &effect, 1).await.unwrap();
    }
    eprintln!("inserted in {:.1?}", started.elapsed());

    // ---- lookups --------------------------------------------------------------
    let mut named = Stats::new("symbols_named");
    let mut component = Stats::new("symbols_in_component");
    let mut by_hash = Stats::new("patch");
    let mut deps = Stats::new("dependents");
    let mut open_for = Stats::new("open_conflicts_for");
    let mut open_all = Stats::new("conflicts(ws, open)");
    let mut sym_get = Stats::new("symbol");
    for n in 0..200 {
        let i = (n * 7919) % SYMBOLS;
        let keys = time(&mut named, g.symbols_named(ws, &sym(i))).await.unwrap();
        assert_eq!(keys, vec![key(i)]);
        let c = time(&mut component, g.symbols_in_component(ws, &format!("c{}", i % COMPONENTS)))
            .await
            .unwrap();
        assert_eq!(c.len(), SYMBOLS / COMPONENTS);
        assert!(time(&mut by_hash, g.patch(ws, &phash(i, 2))).await.unwrap().is_some());
        let d = time(&mut deps, g.dependents(ws, &sym(i))).await.unwrap();
        assert_eq!(d.len(), PATCHES_PER_SYMBOL);
        time(&mut open_for, g.open_conflicts_for(ws, &key(i))).await.unwrap();
        assert!(time(&mut sym_get, g.symbol(ws, &key(i))).await.unwrap().is_some());
        if n % 10 == 0 {
            let open =
                time(&mut open_all, g.conflicts(ws, Some(ConflictState::Open))).await.unwrap();
            assert_eq!(open.len(), CONFLICTS / 2);
        }
    }

    eprintln!("latencies ({} symbols, {} patches):", SYMBOLS, SYMBOLS * PATCHES_PER_SYMBOL);
    let growth = last_tenth.pct(0.5).as_secs_f64() / first_tenth.pct(0.5).as_secs_f64();
    first_tenth.report();
    last_tenth.report();
    eprintln!("  put_patch p50 growth, last/first tenth: {growth:.2}x");
    conflicts_put.report();
    let bounded = [
        named.report(),
        component.report(),
        by_hash.report(),
        deps.report(),
        open_for.report(),
        sym_get.report(),
    ];
    open_all.report(); // returns 500 rows; bounded by the result, not the table
    for p99 in bounded {
        assert!(p99 < Duration::from_millis(50), "a point lookup took {p99:?} at p99");
    }
    assert!(growth < 3.0, "put_patch slowed {growth:.2}x as the tables grew");
}
