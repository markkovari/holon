//! Nine scenarios against a real SurrealDB, and what each one is worth.
//!
//! The unit tests in `lib.rs` pin shapes captured from a live server. These run the
//! statements `surql.rs` builds — the same strings the component sends, not
//! re-spelled ones — against a **pinned** `surrealdb/surrealdb:v3.1.3` and assert
//! what the database actually answers for nine different graph shapes and outcome
//! histories. The half this repo has got wrong before is the half between the
//! statement and the answer (ADR-0061, ADR-0080), and this is that half.
//!
//! Skipped, loudly, when Docker cannot start the database. A skipped test that says
//! so is honest; one that passes because it did nothing is not.
//!
//! `cargo test -p knowledge-memory -- --nocapture scenarios` prints the savings
//! report. The report's COUNTS are asserted; its money is arithmetic over a stated
//! assumption and is asserted by nothing, because this component cannot know what a
//! branch costs.

#![cfg(test)]

use std::io::Write as _;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::surql::{self, EntryWrite};
use crate::{digest, normalise, rrf, weight};

/// The image, PINNED — the same one `reconciler/tests/graph.rs` uses. `latest`
/// would let a server upgrade become a mystery failure in a test that never
/// changed.
const IMAGE: &str = "surrealdb/surrealdb:v3.1.3";

/// What the report multiplies by. **An assumption, not a measurement**: the README
/// records one goal going from a queue to a merge-ready pull request "for a few
/// cents", and a generation is four branches (ADR-0078). Change these two numbers
/// and the money changes; the counts do not.
const BRANCHES_PER_GENERATION: u32 = 4;
const USD_PER_BRANCH: f64 = 0.02;

/// Width of the toy embedding below. Small on purpose: a collision or two is
/// realistic, and a scenario that only passes at 1536 dimensions is not testing
/// the design.
const DIM: usize = 16;

// ------------------------------------------------------------------ the fixture

struct Db {
    name: String,
    port: u16,
}

impl Drop for Db {
    fn drop(&mut self) {
        // `--rm` handles the ordinary exit; this covers a killed run, which is
        // exactly when a leaked container would sit holding a port.
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Db {
    fn start() -> Option<Self> {
        let port = free_port();
        let name = format!("km-scenarios-{port}");
        let ok = Command::new("docker")
            .args(["run", "--rm", "-d", "--name", &name])
            // Loopback only: the container must not be reachable from the network
            // just because a test is running.
            .args(["-p", &format!("127.0.0.1:{port}:8000")])
            .arg(IMAGE)
            .args(["start", "--no-banner", "--user", "root", "--pass", "root"])
            .args(["--bind", "0.0.0.0:8000"])
            .arg("memory")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        let me = Self { name, port };
        // An image pull and a runtime start sit in front of the first request.
        for _ in 0..60 {
            if me.raw("probe", "RETURN 1;").contains("\"OK\"") {
                // And a self-check, because every scenario below reads an absent
                // table as "empty" on purpose: a fixture that is not really talking
                // to a database would make half of them pass while proving nothing.
                let wrote = me.must("probe", "UPSERT probe:⟨x⟩ SET ok = true;");
                assert_eq!(
                    wrote.first().map(|r| r["ok"].clone()),
                    Some(Value::Bool(true)),
                    "the fixture reached {IMAGE} but could not write to it — every \
                     scenario after this would pass by talking to nothing"
                );
                return Some(me);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        None
    }

    /// One statement over HTTP, through curl rather than a new dependency. The
    /// component reaches the same endpoint through `knowledge:graph`, which is
    /// what owns the connection, the credentials and the retry in production.
    fn raw(&self, db: &str, statement: &str) -> String {
        let out = Command::new("curl")
            .args(["-s", "-X", "POST", &format!("http://127.0.0.1:{}/sql", self.port)])
            .args(["-H", "accept: application/json"])
            .args(["-H", "surreal-ns: comp", "-H", &format!("surreal-db: {db}")])
            .args(["-u", "root:root", "--data-binary", "@-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut c| {
                c.stdin.as_mut().unwrap().write_all(statement.as_bytes())?;
                c.wait_with_output()
            });
        out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
    }

    /// The rows a statement answered with, parsed by the component's own reader —
    /// including its rule that a table nobody has written reads as empty.
    fn rows(&self, db: &str, statement: &str) -> Result<Vec<Value>, String> {
        surql::rows(&self.raw(db, statement))
    }

    /// Everything `knowledge:graph`'s `send()` does, in the same order: define the
    /// namespace if the server says it is not there, then resend a conflicted
    /// statement up to four times with no backoff. A conflicted transaction did not
    /// commit, so a resend cannot double-count.
    ///
    /// Mirroring the bootstrap is not tidiness. Without it every statement here
    /// came back "The namespace 'comp' does not exist", which the response reader
    /// maps to *empty* — so the cold-pool scenario passed while talking to nothing
    /// at all. A fixture that cannot tell "no rows" from "no database" proves
    /// nothing, and this is the shape of that bug.
    fn rows_retrying(&self, db: &str, statement: &str) -> Result<Vec<Value>, String> {
        let mut body = self.raw(db, statement);
        if body.contains("does not exist")
            && (body.contains("namespace") || body.contains("database"))
        {
            self.raw(
                db,
                &format!(
                    "DEFINE NAMESPACE IF NOT EXISTS comp; DEFINE DATABASE IF NOT EXISTS {db};"
                ),
            );
            body = self.raw(db, statement);
        }
        let mut attempts = 1;
        // 12, matching `knowledge:graph`'s MAX_ATTEMPTS. This mirrors production
        // deliberately, so when the bound is wrong the scenario is what says so —
        // at 4 it lost a write about one run in three on a loaded machine, which is
        // how the bound came to be raised.
        while body.contains("retry the transaction") && attempts < 12 {
            body = self.raw(db, statement);
            attempts += 1;
        }
        RETRIES.fetch_add(attempts - 1, Ordering::Relaxed);
        surql::rows(&body)
    }

    fn must(&self, db: &str, statement: &str) -> Vec<Value> {
        self.rows_retrying(db, statement)
            .unwrap_or_else(|e| panic!("statement refused: {e}\n{statement}"))
    }
}

static RETRIES: AtomicU32 = AtomicU32::new(0);

/// A deterministic embedding with real similarity structure: token overlap, hashed
/// into `DIM` buckets, L2-normalised. Not semantic — but a scenario about
/// thresholds needs vectors whose closeness it controls, and a provider's would
/// make the assertions untestable.
fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; DIM];
    for token in normalise(text).split(' ').filter(|t| !t.is_empty()) {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in token.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        v[(h % DIM as u64) as usize] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn write_entry(db: &Db, name: &str, handle: &str, ns: &str, goal: &str, text: &str) {
    let w = EntryWrite {
        handle,
        ns,
        text,
        goal,
        env: "env-1",
        attempt: "1",
        score: -1,
        promoted: ns == "patterns",
        // These scenarios pin SurrealQL behaviour, not retrieval policy; the
        // tagged path has its own test (reconciler/tests/tagged.rs).
        tags: &[],
    };
    let vector = embed(&format!("{goal} — {text}"));
    db.must(name, &surql::upsert_entry(&w, Some(&vector), false));
}

/// `already-done`, exactly as the component asks it: the KNN, then the floor.
fn already_done(db: &Db, name: &str, goal: &str, floor: f64) -> Option<(String, f64, u64)> {
    let rows = db.must(name, &surql::already_done_knn(&embed(&normalise(goal))));
    let row = rows.first()?;
    let similarity = surql::similarity_of(row["dist"].as_f64().unwrap_or(1.0));
    if similarity < floor {
        return None;
    }
    Some((
        row["goal"].as_str().unwrap_or_default().to_string(),
        similarity,
        row["evaluations"].as_u64().unwrap_or(0),
    ))
}

/// What `recall` would put in a prompt: KNN candidates, fused with a lexical
/// ordering, weighted by outcomes. The lexical half is `search:index` in
/// production, which has no SurrealDB in it — so the scenarios pass a rank list in
/// rather than pretending to reimplement TF-IDF.
fn recall(
    db: &Db,
    name: &str,
    goal: &str,
    k: u32,
    pools: &[&str],
    lexical: &[&str],
) -> Vec<String> {
    if k == 0 {
        return Vec::new();
    }
    let rows = db.must(name, &surql::knn_entries(&embed(goal), k * 4, pools));
    let mut scored: Vec<(f64, String)> = rows
        .iter()
        .filter(|r| !r["text"].as_str().unwrap_or("").is_empty())
        .enumerate()
        .map(|(dense_rank, r)| {
            let handle = r["id"]
                .as_str()
                .unwrap_or_default()
                .split_once(':')
                .map(|(_, id)| id.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩'))
                .unwrap_or_default()
                .to_string();
            let mut ranks = vec![dense_rank];
            if let Some(r) = lexical.iter().position(|h| *h == handle) {
                ranks.push(r);
            }
            let (uses, wins) = (r["uses"].as_u64().unwrap_or(0), r["wins"].as_u64().unwrap_or(0));
            (rrf(&ranks) * weight(uses, wins), handle)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(k as usize).map(|(_, h)| h).collect()
}

// -------------------------------------------------------------------- the report

#[derive(Default)]
struct Savings {
    goals_asked: u32,
    goals_skipped: u32,
    false_skips: u32,
    roundtrips_naive: u32,
    roundtrips_batched: u32,
    writes_attempted: u32,
    writes_landed_rmw: u32,
    writes_landed_atomic: u32,
    lessons_kept_through_drift: u32,
}

impl Savings {
    fn report(&self) -> String {
        let branches = self.goals_skipped * BRANCHES_PER_GENERATION;
        let mut s = String::new();
        s.push_str(
            "\n=== knowledge:memory — what nine scenarios saved ==========================\n",
        );
        s.push_str(&format!(
            "duplicated work     {}/{} goals answered from a past passing run, {} false skips\n",
            self.goals_skipped, self.goals_asked, self.false_skips
        ));
        s.push_str(&format!(
            "                    → {branches} branches never spawned  ≈ ${:.2} at ${USD_PER_BRANCH:.2}/branch (ASSUMED)\n",
            branches as f64 * USD_PER_BRANCH
        ));
        s.push_str(&format!(
            "retrieval reads     {} round trips → {} for the same candidates ({}x fewer)\n",
            self.roundtrips_naive,
            self.roundtrips_batched,
            self.roundtrips_naive / self.roundtrips_batched.max(1)
        ));
        let resends = RETRIES.load(Ordering::Relaxed);
        s.push_str(&format!(
            "hot-key writes      read-modify-write kept {}/{}; `+=` with retry kept {}/{} ({resends} resend{})\n",
            self.writes_landed_rmw,
            self.writes_attempted,
            self.writes_landed_atomic,
            self.writes_attempted,
            if resends == 1 { "" } else { "s" }
        ));
        s.push_str(&format!(
            "model drift         {} lesson{} kept when the embedding width changed (0 = data loss)\n",
            self.lessons_kept_through_drift,
            if self.lessons_kept_through_drift == 1 { "" } else { "s" }
        ));
        s.push_str("==========================================================================\n");
        s
    }
}

// ----------------------------------------------------------------- the scenarios

#[test]
fn scenarios() {
    let Some(db) = Db::start() else {
        eprintln!(
            "SKIPPED: knowledge-memory scenarios need Docker to start {IMAGE}. \
             Nothing about the database's answers was verified by this run."
        );
        return;
    };
    let mut saved = Savings::default();

    // ---- 1. A cold pool. Nothing has been written; nothing may be skipped.
    //
    // The shape that must not fail: the first `recall` of a project always precedes
    // its first write, so an absent table has to read as "nothing" rather than as
    // "broken".
    let s1 = "s1_cold";
    saved.goals_asked += 1;
    assert_eq!(
        already_done(&db, s1, "slugify a string", 0.9),
        None,
        "an empty graph must not claim any work is done"
    );
    assert!(
        recall(&db, s1, "slugify a string", 5, &["patterns", "errors"], &[]).is_empty(),
        "an empty pool retrieves nothing and reports no error"
    );

    // ---- 2. The same goal, asked twice. One passing evaluation, then a repeat.
    let s2 = "s2_repeat";
    let goal = "make a slug from a title string";
    db.must(
        s2,
        &surql::evaluated(
            &digest(&normalise(goal)),
            goal,
            "run-1",
            1000,
            true,
            "pr/41",
            Some(&embed(&normalise(goal))),
            false,
        ),
    );
    saved.goals_asked += 1;
    let hit = already_done(&db, s2, goal, 0.9).expect("the same goal is done work");
    assert!(hit.1 > 0.999, "an identical goal is similarity ~1.0, got {}", hit.1);
    assert_eq!(hit.2, 1, "one evaluation recorded");
    saved.goals_skipped += 1;

    // ---- 3. A paraphrase is the same work; a stranger is not.
    //
    // This is the scenario that pays for the 0.9 floor. The KNN ALWAYS returns its
    // nearest row — measured `dist: 1.0` for an orthogonal query — so a caller that
    // trusted the query to answer nothing would skip unrelated work and call it
    // done.
    saved.goals_asked += 2;
    let paraphrase = "make a slug from the title string";
    let stranger = "parse a csv file into typed records";
    let near = already_done(&db, s2, paraphrase, 0.9);
    assert!(
        near.is_some(),
        "a paraphrase of done work should reuse it, similarity was {:?}",
        already_done(&db, s2, paraphrase, 0.0).map(|h| h.1)
    );
    saved.goals_skipped += 1;
    let far = already_done(&db, s2, stranger, 0.9);
    assert_eq!(
        far, None,
        "an unrelated goal must not be skipped — the KNN still returned its nearest row"
    );
    if far.is_some() {
        saved.false_skips += 1;
    }

    // ---- 4. A goal that has only ever failed.
    //
    // `passes > 0` is the filter, so three failures leave the work available AND
    // leave the count that says whether a fourth attempt is worth buying.
    let s4 = "s4_failed";
    let hard = "make the flaky integration suite deterministic";
    for (i, run) in ["run-2", "run-3", "run-4"].iter().enumerate() {
        db.must(
            s4,
            &surql::evaluated(
                &digest(&normalise(hard)),
                hard,
                run,
                (i as i32) * 100,
                false,
                "",
                Some(&embed(&normalise(hard))),
                false,
            ),
        );
    }
    saved.goals_asked += 1;
    assert_eq!(already_done(&db, s4, hard, 0.9), None, "three failures are not finished work");
    let row = &db.must(s4, &surql::already_done_exact(&digest(&normalise(hard))));
    assert!(row.is_empty(), "the exact-key path filters on passes > 0 too");
    let all = db.must(
        s4,
        &format!(
            "SELECT count(->evaluated_by) AS evaluations, count(->evaluated_by[WHERE passed = true]) AS passes FROM {};",
            surql::rid(surql::TASKS, &digest(&normalise(hard)))
        ),
    );
    assert_eq!(
        all[0]["evaluations"], 3,
        "every evaluation was recorded, not only the passing ones"
    );
    assert_eq!(all[0]["passes"], 0);
    // And re-reporting one of those runs must not invent a fourth: the verdict
    // edge is keyed by (task, run), so a second report of `run-3` overwrites it.
    // This is what lets the landing path attach a pull request after the fact.
    db.must(
        s4,
        &surql::evaluated(
            &digest(&normalise(hard)),
            hard,
            "run-3",
            150,
            false,
            "",
            Some(&embed(&normalise(hard))),
            false,
        ),
    );
    let after = db.must(
        s4,
        &format!(
            "SELECT count(->evaluated_by) AS evaluations FROM {};",
            surql::rid(surql::TASKS, &digest(&normalise(hard)))
        ),
    );
    assert_eq!(after[0]["evaluations"], 3, "a re-reported run is one verdict, not two");
    // And the per-verdict trail is on the edges, with no run node needed.
    let trail = db.must(
        s4,
        &format!(
            "SELECT ->evaluated_by.score AS scores FROM {};",
            surql::rid(surql::TASKS, &digest(&normalise(hard)))
        ),
    );
    assert_eq!(
        trail[0]["scores"].as_array().map(|a| a.len()),
        Some(3),
        "three verdicts, each recoverable with its score"
    );

    // ---- 5. Two lessons, opposite histories. The outcomes reorder the pool.
    //
    // Nothing asserts a confidence anywhere; the ordering changes only because
    // `attribute` recorded what happened to the runs that read each lesson.
    let s5 = "s5_outcomes";
    let g = "make a slug from a title string";
    write_entry(&db, s5, "errors:bad", "errors", g, "make a slug by lowercasing the title only");
    write_entry(
        &db,
        s5,
        "patterns:good",
        "patterns",
        g,
        "make a slug from a title with char_indices",
    );
    let before = recall(&db, s5, g, 2, &["patterns", "errors"], &[]);
    assert_eq!(before.len(), 2, "both lessons are candidates before any outcome is known");
    for _ in 0..4 {
        db.must(s5, &surql::attribute(&["errors:bad".into()], "run-5", false));
        db.must(s5, &surql::attribute(&["patterns:good".into()], "run-6", true));
    }
    let counters = db
        .must(s5, &format!("SELECT uses, wins FROM {};", surql::rid(surql::ENTRIES, "errors:bad")));
    assert_eq!((counters[0]["uses"].as_u64(), counters[0]["wins"].as_u64()), (Some(4), Some(0)));
    let after = recall(&db, s5, g, 2, &["patterns", "errors"], &[]);
    assert_eq!(
        after[0], "patterns:good",
        "a lesson present when runs passed outranks one present when they failed"
    );
    // The floor still protects it: sinking is not deleting.
    assert!(after.contains(&"errors:bad".to_string()), "sinking is not deletion");

    // ---- 6. Herding, and the control arm.
    //
    // Three branches with identical options read an identical prompt — that is the
    // failure mode ADR-0081 says does not announce itself, and here it is, visible.
    // Varying the pools per branch is what buys the diversity back; `k = 0` is the
    // branch that reads nothing.
    let identical: Vec<Vec<String>> =
        (0..3).map(|_| recall(&db, s5, g, 2, &["patterns", "errors"], &[])).collect();
    assert!(
        identical.windows(2).all(|w| w[0] == w[1]),
        "same goal + same options = same prompt; herding is real"
    );
    let varied = recall(&db, s5, g, 2, &["errors"], &[]);
    assert_ne!(varied, identical[0], "a different pool mix is a different prompt");
    assert!(recall(&db, s5, g, 0, &["patterns"], &[]).is_empty(), "k = 0 is the cold control arm");

    // ---- 7. Twenty branches attributing the same entry.
    //
    // The ADR's table, reproduced as a test: the read-modify-write arm loses
    // writes, the `+=` arm with a bounded retry loses none.
    let s7 = "s7_contention";
    write_entry(&db, s7, "errors:hot", "errors", "one hot lesson", "every branch read this one");
    write_entry(&db, s7, "errors:rmw", "errors", "one hot lesson", "the naive arm");
    const WRITERS: u32 = 20;
    const PER_WRITER: u32 = 3;
    saved.writes_attempted = WRITERS * PER_WRITER;

    std::thread::scope(|scope| {
        for w in 0..WRITERS {
            scope.spawn(move || {
                let db = Db { name: String::new(), port: db.port };
                // Not dropped through `Drop` — this borrows the port only.
                let db = std::mem::ManuallyDrop::new(db);
                for _ in 0..PER_WRITER {
                    db.rows_retrying(
                        s7,
                        &surql::attribute(&["errors:hot".into()], &format!("run-{w}"), true),
                    )
                    .expect("an atomic increment survives contention");
                    // The naive arm, spelled out: read, add one here, write it back.
                    let seen = db
                        .rows(
                            s7,
                            &format!(
                                "SELECT uses FROM {};",
                                surql::rid(surql::ENTRIES, "errors:rmw")
                            ),
                        )
                        .ok()
                        .and_then(|r| r.first().and_then(|v| v["uses"].as_u64()))
                        .unwrap_or(0);
                    let _ = db.rows(
                        s7,
                        &format!(
                            "UPDATE {} SET uses = {};",
                            surql::rid(surql::ENTRIES, "errors:rmw"),
                            seen + 1
                        ),
                    );
                }
            });
        }
    });

    let atomic =
        db.must(s7, &format!("SELECT uses FROM {};", surql::rid(surql::ENTRIES, "errors:hot")));
    let naive =
        db.must(s7, &format!("SELECT uses FROM {};", surql::rid(surql::ENTRIES, "errors:rmw")));
    saved.writes_landed_atomic = atomic[0]["uses"].as_u64().unwrap_or(0) as u32;
    saved.writes_landed_rmw = naive[0]["uses"].as_u64().unwrap_or(0) as u32;
    assert_eq!(
        saved.writes_landed_atomic, saved.writes_attempted,
        "`+=` plus a bounded resend must lose nothing and double-count nothing"
    );
    assert!(
        saved.writes_landed_rmw < saved.writes_attempted,
        "read-modify-write is expected to LOSE writes here — if it did not, this scenario proves nothing"
    );
    // Every branch's edge survived, even the ones whose statement was resent.
    let edges = db.must(
        s7,
        &format!(
            "SELECT count() FROM used_in WHERE in = {} GROUP ALL;",
            surql::rid(surql::ENTRIES, "errors:hot")
        ),
    );
    assert_eq!(
        edges[0]["count"].as_u64(),
        Some(u64::from(saved.writes_attempted)),
        "the edges are the record and none of them is a duplicate"
    );

    // ---- 8. The embedding model changes width.
    let s8 = "s8_drift";
    let w = EntryWrite {
        handle: "errors:drift",
        ns: "errors",
        text: "the gate rejects a bare unwrap",
        goal: "return a result",
        env: "env-1",
        attempt: "1",
        score: -1,
        promoted: false,
        tags: &[],
    };
    db.must(s8, &surql::upsert_entry(&w, Some(&vec![0.5; DIM]), false));
    let refused = db
        .rows(s8, &surql::upsert_entry(&w, Some(&vec![0.5; DIM * 2]), false))
        .expect_err("a wider vector must be refused, not silently mixed in");
    assert!(
        surql::is_dimension_conflict(&refused),
        "the refusal must be recognisable as drift, got: {refused}"
    );
    db.must(s8, &surql::upsert_entry(&w, None, true));
    let kept = db.must(
        s8,
        &format!("SELECT text, dim_conflict FROM {};", surql::rid(surql::ENTRIES, "errors:drift")),
    );
    assert_eq!(kept[0]["text"], "the gate rejects a bare unwrap", "the lesson survives the drift");
    assert_eq!(kept[0]["dim_conflict"], true, "and the drift is queryable rather than invisible");
    saved.lessons_kept_through_drift += 1;

    // ---- 9. Hydrating candidates: one statement or twenty.
    let s9 = "s9_reads";
    let handles: Vec<String> = (0..20).map(|i| format!("errors:h{i}")).collect();
    for h in &handles {
        write_entry(&db, s9, h, "errors", "a goal", &format!("lesson {h}"));
    }
    let naive_start = Instant::now();
    for h in &handles {
        db.must(s9, &format!("SELECT * FROM {};", surql::rid(surql::ENTRIES, h)));
    }
    let naive_ms = naive_start.elapsed().as_millis();
    let batched_start = Instant::now();
    let rows = db.must(s9, &surql::hydrate(&handles));
    let batched_ms = batched_start.elapsed().as_millis();
    assert_eq!(rows.len(), handles.len(), "one statement returns every candidate");
    saved.roundtrips_naive = handles.len() as u32;
    saved.roundtrips_batched = 1;
    assert!(
        batched_ms <= naive_ms,
        "batched hydration took {batched_ms}ms against {naive_ms}ms for {} reads",
        handles.len()
    );

    print!("{}", saved.report());
    assert_eq!(
        saved.false_skips, 0,
        "a false skip is a silent wrong answer and there must be none"
    );
    assert_eq!(saved.goals_skipped, 2, "one exact repeat and one paraphrase");
}

/// Decay: what nobody read goes, what somebody read stays, and what has no date
/// at all survives — the last being the trap, not the feature.
#[test]
fn decay_forgets_the_unread_and_spares_everything_else() {
    let Some(db) = Db::start() else {
        eprintln!("SKIPPED: knowledge-memory scenarios need Docker to start {IMAGE}.");
        return;
    };
    let name = "s11_decay";
    fn entry_at(handle: &str) -> EntryWrite<'_> {
        EntryWrite {
            handle,
            ns: "errors",
            text: "a lesson",
            goal: "a goal",
            env: "env-1",
            attempt: "1",
            score: -1,
            promoted: false,
            // This scenario is about decay, which does not read tags.
            tags: &[],
        }
    }
    for h in ["errors:unread", "errors:earned", "errors:fresh"] {
        db.must(name, &surql::upsert_entry(&entry_at(h), None, false));
    }
    // Age two of them by hand — a test cannot wait a month — and let one earn its
    // place by being read twice.
    db.must(
        name,
        &format!(
            "UPDATE {} SET last_used = time::now() - 40d;",
            surql::rid(surql::ENTRIES, "errors:unread")
        ),
    );
    db.must(
        name,
        &format!(
            "UPDATE {} SET last_used = time::now() - 40d, uses = 5;",
            surql::rid(surql::ENTRIES, "errors:earned")
        ),
    );
    // And one with no date at all: a row from a write path that forgot to stamp it.
    db.must(
        name,
        &format!("UPDATE {} SET last_used = NONE;", surql::rid(surql::ENTRIES, "errors:fresh")),
    );

    let gone = db.must(name, &surql::decay(30, 2));
    assert_eq!(gone.len(), 1, "exactly the unread, old one: {gone:?}");
    assert!(gone[0]["id"].as_str().unwrap_or_default().contains("unread"), "{gone:?}");

    let left = db.must(name, &format!("SELECT id FROM {};", surql::ENTRIES));
    let ids: Vec<String> =
        left.iter().map(|r| r["id"].as_str().unwrap_or_default().to_string()).collect();
    assert!(ids.iter().any(|i| i.contains("earned")), "two reads is earning its place: {ids:?}");
    assert!(
        ids.iter().any(|i| i.contains("fresh")),
        "an entry with no date must NOT be swept — SurrealDB says NONE < any time, \
         so without the guard this deletes rows because of a bug in the writer: {ids:?}"
    );
}

/// The slice-1 cycle exactly as the goal runner drives it: four branch verdicts,
/// then the winner re-reported with the pull request the forge opened.
///
/// Written when the composed e2e counted five verdicts where four were expected.
/// It passed here immediately, which localised the fault in 1.3s instead of 9:
/// the e2e was running a wasm artifact built BEFORE the edge became deterministic.
/// A component change needs `cargo build --target wasm32-wasip2` before the fleet
/// suites mean anything, and a fast harness that disagrees with a slow one is
/// usually telling you they are not running the same code.
#[test]
fn a_re_reported_winner_does_not_invent_a_verdict() {
    let Some(db) = Db::start() else {
        eprintln!("SKIPPED: knowledge-memory scenarios need Docker to start {IMAGE}.");
        return;
    };
    let name = "s10_rereport";
    let goal = "add a retry to the webhook relay";
    let key = digest(&normalise(goal));
    let v = embed(&normalise(goal));
    let runs = [
        ("4242/g0/risk-first", 300, false),
        ("4242/g0/mvp-first", 0, false),
        ("4242/g0/user-first", 1000, true),
        ("4242/g0/cold", 250, false),
    ];
    for (run, score, passed) in runs {
        db.must(name, &surql::evaluated(&key, goal, run, score, passed, "", Some(&v), false));
    }
    let count = |db: &Db| {
        db.must(
            name,
            &format!("SELECT count(->evaluated_by) AS n FROM {};", surql::rid(surql::TASKS, &key)),
        )[0]["n"]
            .as_u64()
            .unwrap_or(0)
    };
    assert_eq!(count(&db), 4, "one verdict per branch");
    db.must(
        name,
        &surql::evaluated(
            &key,
            goal,
            "4242/g0/user-first",
            1000,
            true,
            "https://x.test/pr/7",
            Some(&v),
            false,
        ),
    );
    assert_eq!(count(&db), 4, "the winner re-reported with an artifact is still one verdict");
    let ids = db.must(name, "SELECT id FROM evaluated_by;");
    assert_eq!(ids.len(), 4, "the edge ids that exist: {ids:?}");
}
