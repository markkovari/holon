//! Recall end to end: the real embedding service, the real `Store`, real
//! sentences. Ignored by default because it needs the service
//! (`agent-runtime/embed/server.py`):
//!
//!   HOLON_EMBED_URL=http://127.0.0.1:18102 cargo test --test recall_e2e -- --ignored --nocapture
//!
//! What it checks that the unit tests cannot: that the in-memory index returns
//! the *right* memory for a paraphrased question among a thousand distractors,
//! that agents do not see each other's memories, that a memory is findable the
//! moment it is saved, and what a recall actually costs (embedding included).

use std::time::Instant;

use agent_runtime::embed::Embedder;
use agent_runtime::store::Store;

const FACTS: &[(&str, &str)] = &[
    (
        "The deploy failed because the Redis cache key format changed in release 4.2",
        "why did the deployment break?",
    ),
    (
        "Passport renewal appointment is on 14 March at the consulate, bring two photos",
        "when do I have to go renew my travel document?",
    ),
    (
        "The dentist said to avoid hard food for two days after the filling",
        "what are the restrictions after my tooth work?",
    ),
    (
        "Anna prefers vegetarian restaurants and is allergic to peanuts",
        "where should I take Anna to dinner?",
    ),
    (
        "The quarterly budget review moved to Thursday at 10:00 in room B12",
        "what time is the finance meeting?",
    ),
    (
        "Wi-Fi password for the cabin is stored in the family vault under 'cabin'",
        "how do I get the internet at the cabin?",
    ),
    (
        "Switch the postgres replica to async mode before the nightly backup window",
        "what do I change before the database backup?",
    ),
    (
        "Mom's birthday is 3 June; she likes orchids and hates balloons",
        "what present should I get for my mother?",
    ),
];

const SPORTS: &[&str] = &["rowed", "ran", "cycled", "swam", "lifted"];
const DAYS: &[&str] =
    &["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const FEEL: &[&str] = &["strong", "tired", "great", "sluggish", "fine", "wrecked"];

fn filler(i: usize) -> String {
    format!(
        "{} {}k on {}, felt {} (session {i})",
        SPORTS[i % SPORTS.len()],
        5 + i % 17,
        DAYS[i % DAYS.len()],
        FEEL[(i / 3) % FEEL.len()]
    )
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore = "needs the embedding service: set HOLON_EMBED_URL"]
fn recall_finds_the_right_memory_among_distractors_and_is_fast() {
    let url = std::env::var("HOLON_EMBED_URL").expect("HOLON_EMBED_URL");
    let emb = Embedder::new(&url, "").expect("embedder");
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let docs = |t: &[String]| emb.embed(t, "document");

    for (i, (fact, _)) in FACTS.iter().enumerate() {
        store.remember("me", fact, i as u64).unwrap();
    }
    for i in 0..1000 {
        store.remember("me", &filler(i), 100 + i as u64).unwrap();
    }
    store.remember("other", "Secret: the launch code is 0000", 1).unwrap();

    // The first recall embeds every memory once; later ones must not.
    let (q, t) = (emb.embed(&["warm".to_string()], "query").unwrap().remove(0), Instant::now());
    let first = store.recall_semantic("me", &q, 5, &docs);
    println!("first recall (embeds 1008 memories): {:.0} ms, {} hits", ms(t), first.len());

    let (mut embed_ms, mut search_ms, mut hits_ok) = (vec![], vec![], 0);
    for (fact, question) in FACTS {
        let t = Instant::now();
        let qv = emb.embed(&[question.to_string()], "query").unwrap().remove(0);
        embed_ms.push(ms(t));
        let t = Instant::now();
        let hits = store.recall_semantic("me", &qv, 5, &docs);
        search_ms.push(ms(t));
        let top = hits.first().map(|m| m.text.as_str()).unwrap_or("");
        println!("{} -> {:?}", question, &top[..top.len().min(60)]);
        if top == *fact {
            hits_ok += 1;
        }
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    println!(
        "query embed p50 {:.1} ms, search p50 {:.2} ms, top-1 correct {hits_ok}/{}",
        med(&mut embed_ms),
        med(&mut search_ms),
        FACTS.len()
    );
    assert!(hits_ok >= FACTS.len() - 1, "paraphrased questions must find their fact");

    // Agents do not see each other's memories, even for a query that matches them exactly.
    let qv = emb.embed(&["what is the launch code?".to_string()], "query").unwrap().remove(0);
    let mine = store.recall_semantic("me", &qv, 5, &docs);
    assert!(mine.iter().all(|m| !m.text.contains("launch code")));
    assert!(store.recall_semantic("other", &qv, 5, &docs)[0].text.contains("launch code"));

    // A memory saved now is found on the very next recall.
    store.remember("me", "The garage door code was changed to 4471 yesterday", 9999).unwrap();
    let qv = emb.embed(&["what is the garage door code?".to_string()], "query").unwrap().remove(0);
    let t = Instant::now();
    let hits = store.recall_semantic("me", &qv, 3, &docs);
    println!("save-then-recall (embeds 1 memory): {:.1} ms", ms(t));
    assert!(hits[0].text.contains("4471"));
}
