//! Policy over the graph, lexical index and embeddings: agents observe, only a passing gate promotes.
//! ponytail: a same-width embedding model change slips past the HNSW width guard; add a canary set then.

#[allow(warnings)]
mod bindings;
mod scenarios;
mod surql;

use bindings::exports::knowledge::memory::memory::{
    Entry, Guest, Hit, MemoryError, Namespace, PriorWork, RecallOpts, SubGoal,
};
use bindings::exports::knowledge::memory::promotion::Guest as PromotionGuest;
use bindings::knowledge::graph::store as graph;
use bindings::llm::inference::inference as llm;
use bindings::search::index::index as search;

use serde_json::Value;

struct Component;

const MAX_TEXT: usize = 900;

const DEFAULT_BUDGET: u32 = 1200;

/// Fusion only reorders what the retrievers found, so this is the real recall knob.
const CANDIDATE_FACTOR: u32 = 4;

/// 60 from the RRF paper: flattens rank 1 vs 2 so one confident list cannot dominate.
const RRF_K: f64 = 60.0;

/// High on purpose: skipping work never done is a silent wrong answer; redoing only costs money.
const DEFAULT_SKIP_SIMILARITY: f64 = 0.9;

/// `patterns` is what the swarm believes; raw model output never reaches it.
fn agent_may_write(ns: Namespace) -> bool {
    !matches!(ns, Namespace::Patterns)
}

/// The scale is the fitness function's (ADR-0081), so only the sign is asserted.
fn promotion_allowed(gate_score: i32) -> bool {
    gate_score > 0
}

/// Neutral 0.75, floor 0.5: an entry present in failing runs sinks without anyone deleting it.
fn weight(uses: u64, wins: u64) -> f64 {
    let success = if uses == 0 { 0.5 } else { wins as f64 / uses as f64 };
    0.5 + 0.5 * success
}

fn normalise(goal: &str) -> String {
    goal.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// FNV-1a: a dedup key needs stability, not unforgeability; nothing downstream trusts it.
fn digest(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A row missing `done` reads as not done: redoing is cheaper than silently skipping (ADR-0084).
fn sub_goals(statement: &str) -> Result<Vec<SubGoal>, MemoryError> {
    Ok(ask(statement)?
        .iter()
        .map(|r| SubGoal {
            goal: r["goal"].as_str().unwrap_or_default().to_string(),
            ordinal: r["ordinal"].as_u64().unwrap_or(0) as u32,
            why: r["why"].as_str().unwrap_or_default().to_string(),
            done: r["done"].as_bool().unwrap_or(false),
        })
        .collect())
}

fn ns_name(ns: Namespace) -> &'static str {
    match ns {
        Namespace::Patterns => "patterns",
        Namespace::Solutions => "solutions",
        Namespace::Errors => "errors",
    }
}

fn ns_of(name: &str) -> Namespace {
    match name {
        "patterns" => Namespace::Patterns,
        "errors" => Namespace::Errors,
        // An old or damaged row lands in the untrusted pool, never `patterns`.
        _ => Namespace::Solutions,
    }
}

/// Namespaced: one lesson in `errors` and in `solutions` says different things; must not dedup.
fn handle(ns: Namespace, key: &str) -> String {
    format!("{}:{key}", ns_name(ns))
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((i, _)) => s[..i].to_string(),
    }
}

fn rrf(ranks: &[usize]) -> f64 {
    ranks.iter().map(|r| 1.0 / (RRF_K + *r as f64 + 1.0)).sum()
}

fn trim(mut hits: Vec<Hit>, k: u32, budget: usize) -> Vec<Hit> {
    hits.truncate(k as usize);
    let mut spent = 0usize;
    hits.retain(|h| {
        if spent + h.text.chars().count() > budget {
            return false;
        }
        spent += h.text.chars().count();
        true
    });
    hits
}

struct Stored {
    handle: String,
    ns: Namespace,
    text: String,
    uses: u64,
    wins: u64,
    similarity: Option<f64>,
}

fn stored_of(row: &Value) -> Option<Stored> {
    let full = row["id"].as_str()?;
    // The server re-quotes ids in its preferred form, so quoting is stripped (ADR-0080).
    let h = full
        .split_once(':')
        .map(|(_, id)| id.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩'))?
        .to_string();
    let text = row["text"].as_str().unwrap_or("").to_string();
    // A row with no text (interrupted write, hand edit) must never reach a prompt.
    if text.is_empty() {
        return None;
    }
    Some(Stored {
        handle: h,
        ns: ns_of(row["ns"].as_str().unwrap_or("")),
        text,
        uses: row["uses"].as_u64().unwrap_or(0),
        wins: row["wins"].as_u64().unwrap_or(0),
        similarity: row["dist"].as_f64().map(surql::similarity_of),
    })
}

fn graph_err(e: graph::GraphError) -> MemoryError {
    match e {
        graph::GraphError::Rejected(m) => MemoryError::Rejected(m),
        graph::GraphError::Unavailable(m) => MemoryError::Unavailable(m),
        graph::GraphError::NotConfigured(m) => MemoryError::Unavailable(m),
    }
}

fn search_err(e: search::SearchError) -> MemoryError {
    match e {
        search::SearchError::NotFound => MemoryError::Rejected("not in the index".into()),
        search::SearchError::BackendUnavailable(m) => MemoryError::Unavailable(m),
    }
}

fn ask(statement: &str) -> Result<Vec<Value>, MemoryError> {
    let body = graph::query(statement).map_err(graph_err)?;
    surql::rows(&body).map_err(MemoryError::Rejected)
}

fn dimension_conflict(e: &MemoryError) -> bool {
    matches!(e, MemoryError::Rejected(m) if surql::is_dimension_conflict(m))
}

fn embed_opts() -> llm::Options {
    llm::Options { model: String::new(), temperature: 0, max_tokens: 0, stop: Vec::new(), seed: 0 }
}

/// `None` keeps retrieval lexical; a missing provider must not fail a read or a write.
fn embed(text: &str) -> Option<Vec<f32>> {
    let (_model, embeddings_available) = llm::describe();
    if !embeddings_available {
        return None;
    }
    llm::embed(text, &embed_opts()).ok().filter(|v| !v.is_empty())
}

/// The goal header is most of the retrieval quality (ADR-0081).
fn embeddable(goal: &str, text: &str) -> String {
    format!("{} — {}", goal.trim(), text)
}

/// One statement, no read: `uses += 0` creates or preserves the counter, keeping earned standing.
fn write(e: &Entry, promoted: bool) -> Result<String, MemoryError> {
    let text = truncate(e.text.trim(), MAX_TEXT);
    if text.is_empty() {
        return Err(MemoryError::Refused(
            "an entry with no text is not a lesson — nothing could read it back".into(),
        ));
    }
    let key = match e.key.trim() {
        "" => digest(&normalise(&e.goal)),
        k => k.to_string(),
    };
    let h = handle(e.ns, &key);
    let w = surql::EntryWrite {
        handle: &h,
        ns: ns_name(e.ns),
        text: &text,
        goal: &e.goal,
        env: &e.env,
        attempt: &e.attempt,
        score: e.score,
        promoted,
        tags: &e.tags,
    };

    let vector = embed(&embeddable(&e.goal, &text));
    match ask(&surql::upsert_entry(&w, vector.as_deref(), false)) {
        Ok(_) => {}
        // The embedding width changed: the lesson lands without its vector, flagged.
        Err(err) if dimension_conflict(&err) => {
            ask(&surql::upsert_entry(&w, None, true))?;
        }
        Err(err) => return Err(err),
    }

    // Missing from the lexical index it would never match on tokens, so surfaced.
    search::index_doc(&h, &embeddable(&e.goal, &text), &[format!("ns:{}", ns_name(e.ns))])
        .map_err(search_err)?;

    Ok(h)
}

impl Guest for Component {
    fn observe(e: Entry) -> Result<String, MemoryError> {
        if !agent_may_write(e.ns) {
            return Err(MemoryError::Refused(
                "patterns is written by a passing gate, not by whoever observed something — \
                 record this in solutions and let promotion decide"
                    .into(),
            ));
        }
        write(&e, false)
    }

    fn recall(goal: String, opts: RecallOpts) -> Result<Vec<Hit>, MemoryError> {
        // k = 0 is the control arm, so the shared pool's value can be measured.
        if opts.k == 0 || goal.trim().is_empty() {
            return Ok(Vec::new());
        }
        let budget = if opts.budget == 0 { DEFAULT_BUDGET } else { opts.budget } as usize;
        let pools = if opts.pools.is_empty() {
            vec![Namespace::Patterns, Namespace::Solutions, Namespace::Errors]
        } else {
            opts.pools.clone()
        };
        let per = opts.k.saturating_mul(CANDIDATE_FACTOR);

        let mut lexical: Vec<(String, usize)> = Vec::new();
        for ns in &pools {
            let hits =
                search::query(&goal, search::Mode::Any, &[format!("ns:{}", ns_name(*ns))], per)
                    .map_err(search_err)?;
            for (rank, h) in hits.iter().enumerate() {
                lexical.push((h.id.clone(), rank));
            }
        }

        let mut stored: Vec<Stored> = Vec::new();
        if let Some(q) = embed(&goal) {
            let names: Vec<&str> = pools.iter().map(|n| ns_name(*n)).collect();
            let rows = ask(&surql::knn_entries(&q, per, &names))?;
            stored.extend(rows.iter().filter_map(stored_of));
        }

        // Interface-tag recall crosses applications where goal-text similarity cannot (ADR-0090).
        let mut by_tag: Vec<String> = Vec::new();
        if !opts.tags.is_empty() {
            let names: Vec<&str> = pools.iter().map(|n| ns_name(*n)).collect();
            let rows = ask(&surql::tagged_entries(&opts.tags, opts.k.max(1), &names))?;
            for row in rows.iter().filter_map(stored_of) {
                by_tag.push(row.handle.clone());
                if !stored.iter().any(|s| s.handle == row.handle) {
                    stored.push(row);
                }
            }
        }

        // One statement, not N+1 (ADR-0077).
        let missing: Vec<String> = lexical
            .iter()
            .map(|(h, _)| h.clone())
            .filter(|h| !stored.iter().any(|s| &s.handle == h))
            .collect();
        if !missing.is_empty() {
            let rows = ask(&surql::hydrate(&missing))?;
            stored.extend(rows.iter().filter_map(stored_of));
        }
        if stored.is_empty() {
            return Ok(Vec::new());
        }

        let dense_order: Vec<&String> = {
            let mut with_sim: Vec<&Stored> =
                stored.iter().filter(|s| s.similarity.is_some()).collect();
            with_sim.sort_by(|a, b| b.similarity.unwrap().total_cmp(&a.similarity.unwrap()));
            with_sim.iter().map(|s| &s.handle).collect()
        };
        let mut out: Vec<(f64, Hit)> = Vec::new();
        for s in &stored {
            let mut ranks: Vec<usize> =
                lexical.iter().filter(|(h, _)| *h == s.handle).map(|(_, r)| *r).collect();
            if let Some(rank) = dense_order.iter().position(|h| **h == s.handle) {
                ranks.push(rank);
            }
            let sim = s.similarity.unwrap_or(0.0);
            // Dense-only threshold, else no embedding provider filters everything; a tag match is exempt,
            // being about interfaces rather than text (ADR-0090).
            let matched_by_tag = by_tag.contains(&s.handle);
            if !matched_by_tag
                && s.similarity.is_some()
                && opts.min_similarity > 0.0
                && sim < opts.min_similarity
            {
                continue;
            }
            out.push((
                rrf(&ranks) * weight(s.uses, s.wins),
                Hit {
                    key: s.handle.clone(),
                    ns: s.ns,
                    text: s.text.clone(),
                    similarity: sim,
                    dense: s.similarity.is_some(),
                },
            ));
        }
        // Ties by handle, so two runs over one pool read the same prompt.
        out.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.key.cmp(&b.1.key)));

        Ok(trim(out.into_iter().map(|(_, h)| h).collect(), opts.k, budget))
    }

    fn attribute(keys: Vec<String>, run: String, succeeded: bool) -> Result<(), MemoryError> {
        if run.trim().is_empty() {
            return Err(MemoryError::Refused(
                "an outcome with no run to attribute it to is not evidence".into(),
            ));
        }
        if keys.is_empty() {
            return Ok(());
        }
        // One transaction; `UPDATE` not `UPSERT`, so a deleted handle is not resurrected.
        ask(&surql::attribute(&keys, &run, succeeded)).map(|_| ())
    }

    fn decay(max_age_days: u32, min_uses: u64) -> Result<u32, MemoryError> {
        if max_age_days == 0 {
            return Err(MemoryError::Refused(
                "a max age of zero would forget everything nobody has read yet".into(),
            ));
        }
        let gone = ask(&surql::decay(max_age_days, min_uses))?;
        // search:index cannot remove entries; a hit whose row is gone is filtered on hydration.
        Ok(gone.len() as u32)
    }

    fn evaluated(
        goal: String,
        run: String,
        score: i32,
        passed: bool,
        artifact: String,
    ) -> Result<(), MemoryError> {
        if goal.trim().is_empty() || run.trim().is_empty() {
            return Err(MemoryError::Refused(
                "an evaluation needs a goal and a run — an outcome attached to neither is not evidence"
                    .into(),
            ));
        }
        let key = digest(&normalise(&goal));
        let vector = embed(&normalise(&goal));
        let stmt = |vec: Option<&[f32]>, dim_conflict: bool| {
            surql::evaluated(&key, &goal, &run, score, passed, &artifact, vec, dim_conflict)
        };
        match ask(&stmt(vector.as_deref(), false)) {
            Ok(_) => Ok(()),
            Err(e) if dimension_conflict(&e) => ask(&stmt(None, true)).map(|_| ()),
            Err(e) => Err(e),
        }
    }

    fn decomposed_into(
        parent: String,
        child: String,
        ordinal: u32,
        why: String,
    ) -> Result<(), MemoryError> {
        if parent.trim().is_empty() || child.trim().is_empty() {
            return Err(MemoryError::Refused(
                "a decomposition needs both goals — an edge with an end missing is not a \
                 decomposition"
                    .into(),
            ));
        }
        // Self-decomposition is refused here rather than caught later by a depth bound.
        let (pk, ck) = (digest(&normalise(&parent)), digest(&normalise(&child)));
        if pk == ck {
            return Err(MemoryError::Refused(
                "a goal cannot be a part of itself — these two normalise to the same goal".into(),
            ));
        }
        ask(&surql::decomposed_into(&pk, &parent, &ck, &child, ordinal, &why)).map(|_| ())
    }

    fn parts_of(goal: String) -> Result<Vec<SubGoal>, MemoryError> {
        if goal.trim().is_empty() {
            return Ok(Vec::new());
        }
        sub_goals(&surql::parts_of(&digest(&normalise(&goal))))
    }

    fn parents_of(goal: String) -> Result<Vec<SubGoal>, MemoryError> {
        if goal.trim().is_empty() {
            return Ok(Vec::new());
        }
        sub_goals(&surql::parents_of(&digest(&normalise(&goal))))
    }

    fn already_done(goal: String, min_similarity: f64) -> Result<Option<PriorWork>, MemoryError> {
        if goal.trim().is_empty() {
            return Ok(None);
        }
        let floor = if min_similarity <= 0.0 { DEFAULT_SKIP_SIMILARITY } else { min_similarity };
        let (rows, exact) = match embed(&normalise(&goal)) {
            Some(q) => (ask(&surql::already_done_knn(&q))?, false),
            None => (ask(&surql::already_done_exact(&digest(&normalise(&goal))))?, true),
        };
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let similarity =
            if exact { 1.0 } else { surql::similarity_of(row["dist"].as_f64().unwrap_or(1.0)) };
        if similarity < floor {
            return Ok(None);
        }
        Ok(Some(PriorWork {
            goal: row["goal"].as_str().unwrap_or_default().to_string(),
            similarity,
            score: row["score"].as_i64().unwrap_or(0) as i32,
            run: row["run"].as_str().unwrap_or_default().to_string(),
            artifact: row["artifact"].as_str().unwrap_or_default().to_string(),
            evaluations: row["evaluations"].as_u64().unwrap_or(0) as u32,
        }))
    }
}

impl PromotionGuest for Component {
    fn promote(e: Entry, gate_score: i32) -> Result<String, MemoryError> {
        if !promotion_allowed(gate_score) {
            return Err(MemoryError::Refused(format!(
                "a gate score of {gate_score} did not pass — nothing downstream of a failure is promoted"
            )));
        }
        // Always `patterns`: a hook promoting into `errors` would write a lesson that looks like an observation.
        let promoted = Entry { ns: Namespace::Patterns, score: gate_score, ..e };
        write(&promoted, true)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use surql::{define_index, lit, rid, similarity_of, vec_lit, ENTRIES, TASKS};

    fn rows_of(body: &str) -> Result<Vec<Value>, MemoryError> {
        surql::rows(body).map_err(MemoryError::Rejected)
    }

    #[test]
    fn only_a_gate_writes_what_the_swarm_believes() {
        assert!(!agent_may_write(Namespace::Patterns));
        assert!(agent_may_write(Namespace::Solutions));
        assert!(agent_may_write(Namespace::Errors));
        assert!(promotion_allowed(1));
        assert!(!promotion_allowed(0), "no score is not a pass");
        assert!(!promotion_allowed(-3));
    }

    #[test]
    fn one_goal_spelled_two_ways_is_one_row_per_namespace() {
        let a = handle(Namespace::Errors, &digest(&normalise("Slugify  the TITLE\n")));
        let b = handle(Namespace::Errors, &digest(&normalise("slugify the title")));
        assert_eq!(a, b, "re-learning must reinforce one row");
        let c = handle(Namespace::Solutions, &digest(&normalise("slugify the title")));
        assert_ne!(a, c, "the same lesson in two pools says two different things");
    }

    #[test]
    fn a_caller_supplied_key_cannot_end_the_quoting() {
        assert_eq!(rid(ENTRIES, "errors:abc"), "memory:⟨errors:abc⟩");
        assert_eq!(
            rid(ENTRIES, "errors:x⟩; DELETE memory; --"),
            "memory:⟨errors:x; DELETE memory; --⟩",
            "the closing bracket is the only way out of the quoting"
        );
    }

    #[test]
    fn a_lesson_cannot_carry_surrealql_syntax() {
        assert_eq!(
            lit(r#"he said "stop"; DELETE memory;"#),
            r#""he said \"stop\"; DELETE memory;""#
        );
        assert_eq!(lit("a\nb\\c"), r#""a\nb\\c""#);
        assert_eq!(vec_lit(&[0.5, -1.0]), "[0.5,-1.0]");
    }

    #[test]
    fn a_lesson_is_cut_on_a_character_boundary() {
        assert_eq!(truncate("abc", MAX_TEXT), "abc");
        let long = "é".repeat(MAX_TEXT + 50);
        assert_eq!(truncate(&long, MAX_TEXT).chars().count(), MAX_TEXT);
    }

    #[test]
    fn outcomes_decide_standing_and_nothing_else_does() {
        assert_eq!(weight(0, 0), 0.75, "an unread entry is neutral");
        assert_eq!(weight(4, 4), 1.0, "always present when runs passed");
        assert_eq!(weight(4, 0), 0.5, "always present when runs failed — the floor");
        assert!(weight(4, 1) < weight(4, 3));
    }

    #[test]
    fn fusion_prefers_agreement_over_one_confident_list() {
        assert!(
            rrf(&[2, 3]) > rrf(&[0]),
            "two retrievers agreeing is the evidence RRF exists to reward"
        );
    }

    #[test]
    fn knn_answers_a_distance_and_every_threshold_is_a_similarity() {
        // Captured on v3.1.3: an exact match is distance 0.0; backwards, `min-similarity` keeps everything.
        assert_eq!(similarity_of(0.0), 1.0);
        assert!(similarity_of(0.006_116_265_326_381_098) > 0.99);
        assert!(similarity_of(1.0) <= 0.0);
    }

    #[test]
    fn the_budget_skips_a_lesson_rather_than_halving_it() {
        let hit = |key: &str, len: usize| Hit {
            key: key.into(),
            ns: Namespace::Errors,
            text: "x".repeat(len),
            similarity: 0.0,
            dense: false,
        };
        let kept = trim(vec![hit("a", 30), hit("b", 100), hit("c", 20)], 5, 60);
        let keys: Vec<&str> = kept.iter().map(|h| h.key.as_str()).collect();
        assert_eq!(keys, ["a", "c"], "b does not fit and is skipped, not truncated");
        assert_eq!(trim(vec![hit("a", 1), hit("b", 1)], 1, 1200).len(), 1, "k binds first");
    }

    #[test]
    fn an_empty_pool_does_not_read_as_a_broken_one() {
        let absent =
            r#"[{"status":"ERR","time":"1ms","result":"The table 'memory' does not exist"}]"#;
        assert!(rows_of(absent).unwrap().is_empty());
        let no_ns =
            r#"[{"status":"ERR","time":"1ms","result":"The namespace 'comp' does not exist"}]"#;
        assert!(rows_of(no_ns).unwrap().is_empty());
        assert!(matches!(
            rows_of(r#"[{"status":"ERR","result":"permission denied"}]"#),
            Err(MemoryError::Rejected(_))
        ));
    }

    #[test]
    fn a_multi_statement_body_reads_the_last_result_and_the_first_error() {
        let ok = r#"[{"status":"OK","result":null},{"status":"OK","result":[{"id":"memory:x","text":"a lesson"}]}]"#;
        assert_eq!(rows_of(ok).unwrap().len(), 1);
        let failed_tx = r#"[{"status":"OK","result":null},
            {"status":"ERR","result":"Transaction conflict: Write conflict, retry the transaction. This transaction can be retried"},
            {"status":"ERR","result":"The query was not executed due to a failed transaction"}]"#;
        // A conflict reaching here already survived knowledge:graph's retries.
        assert!(matches!(rows_of(failed_tx), Err(MemoryError::Rejected(_))));
    }

    #[test]
    fn a_changed_embedding_width_is_a_recognisable_rejection() {
        let refused = rows_of(
            r#"[{"kind":"Internal","result":"Incorrect vector dimension (2). Expected a vector of 4 dimension.","status":"ERR"}]"#,
        )
        .expect_err("a dimension mismatch is an error, not an empty result");
        assert!(dimension_conflict(&refused));
        assert!(!dimension_conflict(&MemoryError::Rejected("permission denied".into())));
        assert!(!dimension_conflict(&MemoryError::Unavailable("no route".into())));
    }

    #[test]
    fn a_knn_row_carries_its_similarity_and_a_hydrated_row_does_not() {
        let body = r#"[{"status":"OK","result":[
            {"id":"memory:`errors:9f3a`","ns":"errors","text":"the gate rejects a bare unwrap",
             "goal":"slugify","uses":4,"wins":1,"dim":3,"dist":0.02},
            {"id":"memory:⟨patterns:1⟩","ns":"patterns","text":"split on syntax","uses":0,"wins":0}
        ]}]"#;
        let rows = rows_of(body).unwrap();
        let knn = stored_of(&rows[0]).expect("a row with an id and text is an entry");
        assert_eq!(knn.handle, "errors:9f3a", "the handle round-trips for attribute()");
        assert!(matches!(knn.ns, Namespace::Errors));
        assert_eq!((knn.uses, knn.wins), (4, 1));
        assert!((knn.similarity.unwrap() - 0.98).abs() < 1e-9);
        let hydrated = stored_of(&rows[1]).unwrap();
        assert_eq!(hydrated.similarity, None, "a lexical hit has no cosine to report");
    }

    /// Measured: RELATE to a missing record creates only the edge, so a deleted handle stays deleted.
    #[test]
    fn a_row_with_no_lesson_never_reaches_a_prompt() {
        let row = json!({ "id": "memory:⟨errors:gone⟩", "uses": 3, "wins": 0 });
        assert!(stored_of(&row).is_none());
    }

    #[test]
    fn a_row_missing_its_namespace_is_never_read_as_trusted() {
        let row = json!({ "id": "memory:⟨x⟩", "text": "…" });
        assert!(
            matches!(stored_of(&row).unwrap().ns, Namespace::Solutions),
            "a decoding gap must not promote anything"
        );
    }

    #[test]
    fn what_gets_embedded_carries_its_context_header() {
        assert_eq!(
            embeddable("  slugify a string ", "prefer char_indices"),
            "slugify a string — prefer char_indices",
            "a lesson embedded alone is unfindable"
        );
    }

    #[test]
    fn the_index_definition_is_idempotent_and_carries_the_width() {
        assert_eq!(
            define_index(TASKS, 1536),
            "DEFINE INDEX IF NOT EXISTS task_vec ON task FIELDS vec HNSW DIMENSION 1536 DIST COSINE;"
        );
    }
}
