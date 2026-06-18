//! `search-index` — reference implementation of `search:index`.
//!
//! A small, app-agnostic full-text inverted index that lives entirely in a
//! `wasi:keyvalue` store. No external search engine, no network. The app owns
//! its documents; this owns only the index. Aimed at the long tail of apps
//! whose corpus comfortably fits in a KV store and who don't want to stand up
//! Elasticsearch just to let users search their stuff.
//!
//! Design — an inverted index plus TF-IDF ranking, decomposed across KV keys.
//! All keys live in the `default` bucket; raw token / id / tag strings are
//! folded into kv-legal keys with the same byte-escape scheme as the sibling
//! idempotency-guard (`_XX` for any non `[A-Za-z0-9-/=]` byte), each behind a
//! namespace prefix so the four key spaces never collide:
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
//! Tokenizer: lowercase, fold every non-ascii-alphanumeric byte to a space,
//! split on whitespace, drop empties and tokens shorter than 2 chars. A doc's
//! *term set* is deduped, but we keep the per-doc term frequency for ranking.
//!
//! Ranking (`query`): gather candidate docs from the query terms' posting
//! lists (ANY = union, ALL = intersection across every query term). An optional
//! `tags` filter intersects candidates with docs carrying ALL given tags. Each
//! candidate is scored with TF-IDF: for every query term `t` matched in doc `d`,
//!   score(d) += tf(d,t) * idf(t),   idf(t) = ln((N + 1) / (df(t) + 1)) + 1
//! where N = `si_count` and df(t) = number of docs in t's posting list. Hits are
//! returned sorted by score descending, truncated to `limit`.
//!
//! Single-writer / best-effort caveat: the posting lists, tag indexes and the
//! forward record are maintained with read-modify-write sequences against the
//! KV store, which `wasi:keyvalue@0.2.0-draft` cannot make atomic (it exposes no
//! compare-and-swap, only `increment`). Concurrent writers to the *same* term or
//! tag can therefore lose an update by interleaving. This is fine under a single
//! logical writer per index (the common case); a multi-writer deployment would
//! need external serialization. Reads/queries are always consistent with
//! whatever the store currently holds.

#[allow(warnings)]
mod bindings;

use bindings::exports::search::index::index::{Guest, Hit, Mode, SearchError};
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";

// ---- key namespacing -----------------------------------------------------

/// Escape a raw string to kv-legal bytes (same scheme as idempotency-guard's
/// `id_key`), behind the given namespace prefix.
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

// ---- tokenizer -----------------------------------------------------------

/// Lowercase, fold non-ascii-alnum to spaces, split, drop empties and len < 2.
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
    folded
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_string())
        .collect()
}

/// Tokenize and reduce to per-doc term frequencies (deduped term set with counts).
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

// ---- kv helpers ----------------------------------------------------------

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
    bucket
        .delete(key)
        .map_err(|e| SearchError::BackendUnavailable(format!("delete {key}: {e:?}")))
}

// ---- count maintenance ---------------------------------------------------

fn read_count(bucket: &kv::Bucket) -> Result<u64, SearchError> {
    Ok(get_string(bucket, COUNT_KEY)?
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0))
}

fn write_count(bucket: &kv::Bucket, n: u64) -> Result<(), SearchError> {
    set_string(bucket, COUNT_KEY, &n.to_string())
}

// ---- posting-list maintenance --------------------------------------------

/// Add (or update) `id`'s entry in `token`'s posting list with the given tf.
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

/// Remove `id`'s entry from `token`'s posting list (deleting the key if empty).
fn posting_remove(bucket: &kv::Bucket, token: &str, id: &str) -> Result<(), SearchError> {
    let key = term_key(token);
    let existing = match get_string(bucket, &key)? {
        Some(s) => s,
        None => return Ok(()),
    };
    let lines: Vec<&str> = existing
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| posting_docid(l) != id)
        .collect();
    if lines.is_empty() {
        delete_key(bucket, &key)
    } else {
        set_string(bucket, &key, &lines.join("\n"))
    }
}

/// The docid portion of a "{docid}:{tf}" posting entry (tf may itself be absent).
fn posting_docid(line: &str) -> &str {
    match line.rfind(':') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Parse a "{docid}:{tf}" posting entry into (docid, tf).
fn parse_posting(line: &str) -> (String, u64) {
    match line.rfind(':') {
        Some(i) => {
            let tf = line[i + 1..].parse::<u64>().unwrap_or(1);
            (line[..i].to_string(), tf)
        }
        None => (line.to_string(), 1),
    }
}

// ---- tag-index maintenance -----------------------------------------------

fn tag_add(bucket: &kv::Bucket, tag: &str, id: &str) -> Result<(), SearchError> {
    let key = tag_key(tag);
    let existing = get_string(bucket, &key)?.unwrap_or_default();
    let mut ids: Vec<String> = existing
        .lines()
        .filter(|l| !l.is_empty() && *l != id)
        .map(|l| l.to_string())
        .collect();
    ids.push(id.to_string());
    set_string(bucket, &key, &ids.join("\n"))
}

fn tag_remove(bucket: &kv::Bucket, tag: &str, id: &str) -> Result<(), SearchError> {
    let key = tag_key(tag);
    let existing = match get_string(bucket, &key)? {
        Some(s) => s,
        None => return Ok(()),
    };
    let ids: Vec<&str> = existing
        .lines()
        .filter(|l| !l.is_empty() && *l != id)
        .collect();
    if ids.is_empty() {
        delete_key(bucket, &key)
    } else {
        set_string(bucket, &key, &ids.join("\n"))
    }
}

fn tag_docids(bucket: &kv::Bucket, tag: &str) -> Result<Vec<String>, SearchError> {
    Ok(get_string(bucket, &tag_key(tag))?
        .map(|s| {
            s.lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        })
        .unwrap_or_default())
}

// ---- forward record ------------------------------------------------------

/// (tokens-with-tf, tags) of a doc's stored forward record, if present.
fn load_forward(
    bucket: &kv::Bucket,
    id: &str,
) -> Result<Option<(Vec<(String, u64)>, Vec<String>)>, SearchError> {
    let raw = match get_string(bucket, &doc_key(id))? {
        Some(s) => s,
        None => return Ok(None),
    };
    let mut lines = raw.lines();
    let tok_line = lines.next().unwrap_or("");
    let tag_line = lines.next().unwrap_or("");
    let tokens: Vec<(String, u64)> = tok_line
        .split_whitespace()
        .map(parse_posting)
        .collect();
    let tags: Vec<String> = tag_line
        .split_whitespace()
        .map(|t| t.to_string())
        .collect();
    Ok(Some((tokens, tags)))
}

fn store_forward(
    bucket: &kv::Bucket,
    id: &str,
    terms: &[(String, u64)],
    tags: &[String],
) -> Result<(), SearchError> {
    let tok_line = terms
        .iter()
        .map(|(t, tf)| format!("{t}:{tf}"))
        .collect::<Vec<_>>()
        .join(" ");
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

// ---- guest ---------------------------------------------------------------

impl Guest for Component {
    fn index_doc(id: String, text: String, tags: Vec<String>) -> Result<(), SearchError> {
        let bucket = open()?;

        // Replace semantics: if the doc already exists, strip the postings and
        // tag entries it previously contributed before re-indexing. This is a
        // replace, so `si_count` is left untouched here.
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
        // Idempotent: an absent doc is a no-op success.
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
                .map(|s| {
                    s.lines()
                        .filter(|l| !l.is_empty())
                        .map(parse_posting)
                        .collect()
                })
                .unwrap_or_default();

            let df = postings.len() as u64;
            if df == 0 {
                continue;
            }
            // idf(t) = ln((N + 1) / (df + 1)) + 1
            let idf = (((n + 1) as f64) / ((df + 1) as f64)).ln() + 1.0;

            for (doc, tf) in postings {
                let contrib = (tf as f64) * idf;
                if let Some(slot) = cands.iter_mut().find(|(d, _)| *d == doc) {
                    slot.1.score += contrib;
                    slot.1.matched_terms += 1;
                } else {
                    cands.push((
                        doc,
                        Cand {
                            score: contrib,
                            matched_terms: 1,
                        },
                    ));
                }
            }
        }

        // mode = all -> a candidate must have matched every distinct query term.
        if matches!(mode, Mode::All) {
            let need = terms.len() as u32;
            cands.retain(|(_, c)| c.matched_terms >= need);
        }

        // tag filter -> candidate must carry ALL given tags.
        if !tags.is_empty() {
            let mut allowed: Option<Vec<String>> = None;
            for tag in &tags {
                let ids = tag_docids(&bucket, tag)?;
                allowed = Some(match allowed {
                    None => ids,
                    Some(prev) => prev.into_iter().filter(|d| ids.contains(d)).collect(),
                });
                // Early exit: empty intersection -> no hits possible.
                if allowed.as_ref().map(|v| v.is_empty()).unwrap_or(false) {
                    return Ok(Vec::new());
                }
            }
            if let Some(allowed) = allowed {
                cands.retain(|(d, _)| allowed.contains(d));
            }
        }

        // Sort by score descending; truncate to limit.
        cands.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let hits = cands
            .into_iter()
            .take(limit as usize)
            .map(|(id, c)| Hit {
                id,
                score: c.score,
            })
            .collect();
        Ok(hits)
    }

    fn doc_count() -> Result<u64, SearchError> {
        let bucket = open()?;
        read_count(&bucket)
    }
}

bindings::export!(Component with_types_in bindings);
