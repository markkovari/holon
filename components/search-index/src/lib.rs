//! `search-index` — let users search their own data — a full-text inverted index with ranked results
//!
//! A small full-text inverted index living entirely in a `wasi:keyvalue` store; the app owns its
//! documents, this owns only the index.
//!
//! TF-IDF ranking over an inverted index. All keys live in the `default` bucket; raw strings are
//! folded into kv-legal keys with the byte-escape scheme of idempotency-guard (`_XX`), each
//! behind a namespace prefix:
//!
//!   si_count            -> ASCII integer: number of documents indexed.
//!   si_t_{token}        -> posting list, newline-joined "{docid}:{tf}" entries
//!                          (tf = term frequency of `token` within that doc).
//!   si_d_{docid}        -> forward record for a doc, two lines:
//!                            line 1: space-joined "tok:tf ..." (the doc's terms)
//!                            line 2: space-joined tags
//!                          The forward record is what lets re-index / remove
//!                          clean up the postings a doc previously contributed.
//!   si_tag_{tag}        -> newline-joined docids carrying that tag.
//!
//! Tokenizer: lowercase, fold non-ascii-alphanumeric bytes to spaces, split, drop tokens under 2
//! chars; the term set is deduped but per-doc tf is kept.
//! Ranking (`query`): candidates come from the query terms' posting lists (ANY = union, ALL =
//! intersection), optionally intersected with docs carrying ALL given `tags`. Score is TF-IDF,
//! score(d) += tf(d,t) * idf(t), idf(t) = ln((N + 1) / (df(t) + 1)) + 1, sorted descending,
//! truncated to `limit`.
//!
//! Single-writer caveat: posting lists, tag indexes and forward records are read-modify-write,
//! which `wasi:keyvalue@0.2.0-draft` cannot make atomic (no compare-and-swap), so concurrent
//! writers to the same term or tag can lose an update. Fine for one logical writer per index;
//! reads are always consistent with the store.

#[allow(warnings)]
mod bindings;

use bindings::exports::search::index::index::{Guest, Hit, Mode, SearchError};
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";

fn safe_key(prefix: &str, raw: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + raw.len() + 4);
    out.push_str(prefix);
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn term_key(token: &str) -> String {
    safe_key("si_t_", token)
}

fn doc_key(id: &str) -> String {
    safe_key("si_d_", id)
}

fn tag_key(tag: &str) -> String {
    safe_key("si_tag_", tag)
}

const COUNT_KEY: &str = "si_count";

fn tokenize(text: &str) -> Vec<String> {
    let folded: String = text
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() {
                c
            } else {
                ' '
            }
        })
        .collect();
    folded.split_whitespace().filter(|t| t.len() >= 2).map(|t| t.to_string()).collect()
}

fn term_freqs(text: &str) -> Vec<(String, u64)> {
    let mut acc: Vec<(String, u64)> = Vec::new();
    'next: for tok in tokenize(text) {
        for entry in acc.iter_mut() {
            if entry.0 == tok {
                entry.1 += 1;
                continue 'next;
            }
        }
        acc.push((tok, 1));
    }
    acc
}

fn open() -> Result<kv::Bucket, SearchError> {
    kv::open(BUCKET).map_err(|e| SearchError::BackendUnavailable(format!("open: {e:?}")))
}

fn get_string(bucket: &kv::Bucket, key: &str) -> Result<Option<String>, SearchError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| SearchError::BackendUnavailable(format!("value not utf-8: {key}")))?;
            Ok(Some(s))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(SearchError::BackendUnavailable(format!("get {key}: {e:?}"))),
    }
}

fn set_string(bucket: &kv::Bucket, key: &str, value: &str) -> Result<(), SearchError> {
    bucket
        .set(key, value.as_bytes())
        .map_err(|e| SearchError::BackendUnavailable(format!("set {key}: {e:?}")))
}

fn delete_key(bucket: &kv::Bucket, key: &str) -> Result<(), SearchError> {
    bucket.delete(key).map_err(|e| SearchError::BackendUnavailable(format!("delete {key}: {e:?}")))
}

fn read_count(bucket: &kv::Bucket) -> Result<u64, SearchError> {
    Ok(get_string(bucket, COUNT_KEY)?.and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0))
}

fn write_count(bucket: &kv::Bucket, n: u64) -> Result<(), SearchError> {
    set_string(bucket, COUNT_KEY, &n.to_string())
}

fn posting_add(bucket: &kv::Bucket, token: &str, id: &str, tf: u64) -> Result<(), SearchError> {
    let key = term_key(token);
    let existing = get_string(bucket, &key)?.unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| posting_docid(l) != id)
        .map(|l| l.to_string())
        .collect();
    lines.push(format!("{id}:{tf}"));
    set_string(bucket, &key, &lines.join("\n"))
}

fn posting_remove(bucket: &kv::Bucket, token: &str, id: &str) -> Result<(), SearchError> {
    let key = term_key(token);
    let existing = match get_string(bucket, &key)? {
        Some(s) => s,
        None => return Ok(()),
    };
    let lines: Vec<&str> =
        existing.lines().filter(|l| !l.is_empty()).filter(|l| posting_docid(l) != id).collect();
    if lines.is_empty() {
        delete_key(bucket, &key)
    } else {
        set_string(bucket, &key, &lines.join("\n"))
    }
}

fn posting_docid(line: &str) -> &str {
    match line.rfind(':') {
        Some(i) => &line[..i],
        None => line,
    }
}

fn parse_posting(line: &str) -> (String, u64) {
    match line.rfind(':') {
        Some(i) => {
            let tf = line[i + 1..].parse::<u64>().unwrap_or(1);
            (line[..i].to_string(), tf)
        }
        None => (line.to_string(), 1),
    }
}

fn tag_add(bucket: &kv::Bucket, tag: &str, id: &str) -> Result<(), SearchError> {
    let key = tag_key(tag);
    let existing = get_string(bucket, &key)?.unwrap_or_default();
    let mut ids: Vec<String> =
        existing.lines().filter(|l| !l.is_empty() && *l != id).map(|l| l.to_string()).collect();
    ids.push(id.to_string());
    set_string(bucket, &key, &ids.join("\n"))
}

fn tag_remove(bucket: &kv::Bucket, tag: &str, id: &str) -> Result<(), SearchError> {
    let key = tag_key(tag);
    let existing = match get_string(bucket, &key)? {
        Some(s) => s,
        None => return Ok(()),
    };
    let ids: Vec<&str> = existing.lines().filter(|l| !l.is_empty() && *l != id).collect();
    if ids.is_empty() {
        delete_key(bucket, &key)
    } else {
        set_string(bucket, &key, &ids.join("\n"))
    }
}

fn tag_docids(bucket: &kv::Bucket, tag: &str) -> Result<Vec<String>, SearchError> {
    Ok(get_string(bucket, &tag_key(tag))?
        .map(|s| s.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect())
        .unwrap_or_default())
}

type ForwardRecord = (Vec<(String, u64)>, Vec<String>);

fn load_forward(bucket: &kv::Bucket, id: &str) -> Result<Option<ForwardRecord>, SearchError> {
    let raw = match get_string(bucket, &doc_key(id))? {
        Some(s) => s,
        None => return Ok(None),
    };
    let mut lines = raw.lines();
    let tok_line = lines.next().unwrap_or("");
    let tag_line = lines.next().unwrap_or("");
    let tokens: Vec<(String, u64)> = tok_line.split_whitespace().map(parse_posting).collect();
    let tags: Vec<String> = tag_line.split_whitespace().map(|t| t.to_string()).collect();
    Ok(Some((tokens, tags)))
}

fn store_forward(
    bucket: &kv::Bucket,
    id: &str,
    terms: &[(String, u64)],
    tags: &[String],
) -> Result<(), SearchError> {
    let tok_line = terms.iter().map(|(t, tf)| format!("{t}:{tf}")).collect::<Vec<_>>().join(" ");
    let tag_line = tags.join(" ");
    set_string(bucket, &doc_key(id), &format!("{tok_line}\n{tag_line}"))
}

/// Tear down all postings + tag entries a doc currently contributes, using its
/// forward record. Does NOT touch `si_count` or the forward record itself —
/// callers decide whether this is a replace (re-index) or a real removal.
fn cleanup_postings(bucket: &kv::Bucket, id: &str) -> Result<bool, SearchError> {
    let (tokens, tags) = match load_forward(bucket, id)? {
        Some(v) => v,
        None => return Ok(false),
    };
    for (tok, _tf) in &tokens {
        posting_remove(bucket, tok, id)?;
    }
    for tag in &tags {
        tag_remove(bucket, tag, id)?;
    }
    Ok(true)
}

impl Guest for Component {
    fn index_doc(id: String, text: String, tags: Vec<String>) -> Result<(), SearchError> {
        let bucket = open()?;

        // Replace semantics: strip the postings and tags the doc previously contributed before
        // re-indexing; `si_count` is left untouched.
        let existed = cleanup_postings(&bucket, &id)?;

        let terms = term_freqs(&text);
        for (tok, tf) in &terms {
            posting_add(&bucket, tok, &id, *tf)?;
        }
        for tag in &tags {
            tag_add(&bucket, tag, &id)?;
        }
        store_forward(&bucket, &id, &terms, &tags)?;

        if !existed {
            let n = read_count(&bucket)?;
            write_count(&bucket, n.saturating_add(1))?;
        }
        Ok(())
    }

    fn remove(id: String) -> Result<(), SearchError> {
        let bucket = open()?;
        if !cleanup_postings(&bucket, &id)? {
            return Ok(());
        }
        delete_key(&bucket, &doc_key(&id))?;
        let n = read_count(&bucket)?;
        write_count(&bucket, n.saturating_sub(1))?;
        Ok(())
    }

    fn query(
        query: String,
        mode: Mode,
        tags: Vec<String>,
        limit: u32,
    ) -> Result<Vec<Hit>, SearchError> {
        let bucket = open()?;
        let q_terms = tokenize(&query);
        if q_terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        // Dedup query terms (a repeated query word shouldn't double-count idf).
        let mut terms: Vec<String> = Vec::new();
        for t in q_terms {
            if !terms.contains(&t) {
                terms.push(t);
            }
        }

        let n = read_count(&bucket)?;

        // For each distinct query term, load its posting list once: the set of
        // (docid, tf) plus df for idf. Accumulate per-doc contributions.
        struct Cand {
            score: f64,
            matched_terms: u32,
        }
        let mut cands: Vec<(String, Cand)> = Vec::new();

        for term in &terms {
            let postings: Vec<(String, u64)> = get_string(&bucket, &term_key(term))?
                .map(|s| s.lines().filter(|l| !l.is_empty()).map(parse_posting).collect())
                .unwrap_or_default();

            let df = postings.len() as u64;
            if df == 0 {
                continue;
            }
            let idf = (((n + 1) as f64) / ((df + 1) as f64)).ln() + 1.0;

            for (doc, tf) in postings {
                let contrib = (tf as f64) * idf;
                if let Some(slot) = cands.iter_mut().find(|(d, _)| *d == doc) {
                    slot.1.score += contrib;
                    slot.1.matched_terms += 1;
                } else {
                    cands.push((doc, Cand { score: contrib, matched_terms: 1 }));
                }
            }
        }

        if matches!(mode, Mode::All) {
            let need = terms.len() as u32;
            cands.retain(|(_, c)| c.matched_terms >= need);
        }

        if !tags.is_empty() {
            let mut allowed: Option<Vec<String>> = None;
            for tag in &tags {
                let ids = tag_docids(&bucket, tag)?;
                allowed = Some(match allowed {
                    None => ids,
                    Some(prev) => prev.into_iter().filter(|d| ids.contains(d)).collect(),
                });
                if allowed.as_ref().map(|v| v.is_empty()).unwrap_or(false) {
                    return Ok(Vec::new());
                }
            }
            if let Some(allowed) = allowed {
                cands.retain(|(d, _)| allowed.contains(d));
            }
        }

        cands.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let hits = cands
            .into_iter()
            .take(limit as usize)
            .map(|(id, c)| Hit { id, score: c.score })
            .collect();
        Ok(hits)
    }

    fn doc_count() -> Result<u64, SearchError> {
        let bucket = open()?;
        read_count(&bucket)
    }
}

bindings::export!(Component with_types_in bindings);
