//! Browsing: the query string, search, keyset paging, and the `state` lookups
//! every list route uses (CONTRACT.md "Browsing and the archive").
//!
//! **Paging is keyset, not offset.** Every list is a total order over
//! `(sort key, record id)` — the key is a unix time, the id a ULID, so no two
//! items tie. The cursor is the last item of a page, and the next page is every
//! item strictly after it in that order. An item added or removed between two
//! requests therefore never shifts another one into a duplicate or a gap: a new
//! item that sorts before the cursor is simply not on the pages still to come.
//!
//! The cursor is opaque to clients (hex over `v1|<scope>|<key>|<id>`) and bound
//! to the list it came from: one handed to another view, or tampered with, is
//! `400 bad_cursor` rather than a silently wrong page.
//!
//! **Where the page is cut.** `records:store` has equality indexes and no ordered
//! (range) index, so a route narrows by index — `state`, or the caller's own
//! records by `user` — and cuts the page in memory from what that returned. That
//! loads the matching records, not the collection; see each route for which
//! index answers it.
//!
//! **`state` indexes and older records.** Indexes are chosen when a record is
//! created and can never be added to it later (`records:store` `update` keeps the
//! create-time index list). `competitions` were always indexed by `state`;
//! `journeys` and `quests` are from this change on. Their older records are
//! found once by a scan ([`by_state`], [`legacy_ids`]) and remembered in
//! `index_backfill`, so the scan is not repeated per request.

use crate::bindings::records::store::store as records;
use crate::progress::{find, list_all, load, str_of};
use crate::Reply;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 100;
/// Longest `q` honoured; a longer one is cut (a title search, not a document).
const MAX_Q: usize = 200;
/// Every state a journey, quest or competition can be in.
pub const STATES: &[&str] = &["draft", "published", "archived"];
/// Remembers, per collection, the records created before it was indexed by `state`.
const BACKFILL: &str = "index_backfill";

// ---- the query string ------------------------------------------------------------

/// `%XX` and `+` decoded; invalid escapes kept as they are.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One query parameter of a `path?query`, decoded. The first occurrence wins.
pub fn param(path: &str, key: &str) -> Option<String> {
    let q = path.split_once('?').map(|(_, q)| q)?;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

/// `?view=` (or `?state=`) against the allowed values; absent or empty = `default`.
pub fn choice(path: &str, key: &str, allowed: &[&str], default: &str) -> Result<String, Reply> {
    match param(path, key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        None => Ok(default.to_string()),
        Some(v) if allowed.contains(&v.as_str()) => Ok(v),
        Some(v) => Err(Reply::json(
            400,
            json!({"error": format!("bad_{key}"), "detail": format!("{key} must be one of {}; got {v:?}", allowed.join(", "))}),
        )),
    }
}

// ---- search ------------------------------------------------------------------------

/// A title search: trimmed, lower-cased, at most 200 characters; None = no filter.
pub fn search(path: &str) -> Option<String> {
    let q = param(path, "q")?;
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    Some(q.chars().take(MAX_Q).collect::<String>().to_lowercase())
}

/// Case-insensitive substring match; no search matches everything.
pub fn matches(title: &str, q: Option<&str>) -> bool {
    match q {
        None => true,
        Some(q) => title.to_lowercase().contains(q),
    }
}

// ---- paging -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Order {
    /// Smallest key first (e.g. soonest deadline).
    Asc,
    /// Largest key first (newest).
    Desc,
}

/// A page request: `limit` and the decoded `after` position.
#[derive(Debug, PartialEq)]
pub struct Page {
    pub limit: usize,
    pub after: Option<(u64, String)>,
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<String> {
    if s.len() % 2 != 0 || s.len() > 1024 {
        return None;
    }
    let bytes: Option<Vec<u8>> =
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect();
    String::from_utf8(bytes?).ok()
}

/// The opaque cursor for position `(key, id)` in list `scope`.
pub fn cursor(scope: &str, key: u64, id: &str) -> String {
    hex(&format!("v1|{scope}|{key}|{id}"))
}

/// `(key, id)` out of a cursor minted for `scope`; None for anything else.
pub fn decode_cursor(scope: &str, c: &str) -> Option<(u64, String)> {
    let s = unhex(c)?;
    let mut parts = s.splitn(4, '|');
    if parts.next()? != "v1" || parts.next()? != scope {
        return None;
    }
    let key = parts.next()?.parse().ok()?;
    let id = parts.next()?;
    crate::progress::valid_id(id).then(|| (key, id.to_string()))
}

/// `limit` (default 20, at most 100 — more is cut to 100; 0 or not a number is
/// `400 bad_limit`) and `after` (`400 bad_cursor` unless minted for `scope`).
pub fn page_of(path: &str, scope: &str) -> Result<Page, Reply> {
    let limit = match param(path, "limit").map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        None => DEFAULT_LIMIT,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if n >= 1 => n.min(MAX_LIMIT),
            _ => return Err(Reply::json(400, json!({"error": "bad_limit", "detail": "limit must be a whole number from 1 to 100"}))),
        },
    };
    let after = match param(path, "after").filter(|v| !v.is_empty()) {
        None => None,
        Some(c) => match decode_cursor(scope, &c) {
            Some(p) => Some(p),
            None => return Err(Reply::json(400, json!({"error": "bad_cursor", "detail": "after is not a cursor this list handed out"}))),
        },
    };
    Ok(Page { limit, after })
}

fn cmp_pos(order: Order, a: &(u64, String), b: &(u64, String)) -> std::cmp::Ordering {
    let o = a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1));
    match order {
        Order::Asc => o,
        Order::Desc => o.reverse(),
    }
}

/// Sort `items` by `(key, id)` in `order`, keep those strictly after the
/// cursor, and cut `limit`. Returns the page and the cursor for the next one
/// (None when this was the last).
pub fn cut<T>(
    mut items: Vec<T>,
    pos: impl Fn(&T) -> (u64, String),
    order: Order,
    page: &Page,
    scope: &str,
) -> (Vec<T>, Option<String>) {
    items.sort_by(|a, b| cmp_pos(order, &pos(a), &pos(b)));
    let mut rest: Vec<T> = match &page.after {
        None => items,
        Some(after) => items
            .into_iter()
            .filter(|x| cmp_pos(order, &pos(x), after) == std::cmp::Ordering::Greater)
            .collect(),
    };
    let more = rest.len() > page.limit;
    rest.truncate(page.limit);
    let next = if more {
        rest.last().map(|x| {
            let (k, id) = pos(x);
            cursor(scope, k, &id)
        })
    } else {
        None
    };
    (rest, next)
}

/// A record's `(key, id)`: the first of `keys` that is set, else 0.
pub fn pos_of(m: &Map<String, Value>, keys: &[&str]) -> (u64, String) {
    let k = keys.iter().find_map(|k| m.get(*k).and_then(Value::as_u64)).unwrap_or(0);
    (k, str_of(m, "id").to_string())
}

// ---- state lookups ---------------------------------------------------------------------

/// Ids in `collection` that were created without a `state` index — found by one
/// scan the first time they are asked for, then read from `index_backfill`.
///
/// Why this is enough: records created from now on are always indexed, and a
/// record that was not can never be (the index list is fixed at create), so the
/// set only ever loses members (deleted records, which [`by_state`] skips). A
/// record whose index write had not landed yet during the scan is counted here
/// too — harmless, it is loaded by id and de-duplicated.
pub fn legacy_ids(collection: &str) -> Result<Vec<String>, Reply> {
    if let Some(m) = find(BACKFILL, "collection", collection)?.into_iter().min_by(|a, b| {
        str_of(a, "id").cmp(str_of(b, "id"))
    }) {
        return Ok(ids_in(&m, "legacy"));
    }
    let all = list_all(collection)?;
    let mut indexed = HashSet::new();
    for s in STATES {
        for m in find(collection, "state", s)? {
            indexed.insert(str_of(&m, "id").to_string());
        }
    }
    let legacy: Vec<String> = all
        .iter()
        .map(|m| str_of(m, "id").to_string())
        .filter(|id| !indexed.contains(id))
        .collect();
    let rec = json!({"collection": collection, "field": "state", "legacy": legacy,
                     "at": crate::now_secs()});
    // Two first requests racing both write one; they agree, and the lowest id is read.
    let _ = records::create(BACKFILL, &rec.to_string(), &["collection".to_string()]);
    Ok(legacy)
}

fn ids_in(m: &Map<String, Value>, key: &str) -> Vec<String> {
    m.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Every record of `collection` in `state`: the `state` index, plus the records
/// from before it existed ([`legacy_ids`]), each re-checked.
pub fn by_state(collection: &str, state: &str) -> Result<Vec<Map<String, Value>>, Reply> {
    let mut out = find(collection, "state", state)?;
    let seen: HashSet<String> = out.iter().map(|m| str_of(m, "id").to_string()).collect();
    for id in legacy_ids(collection)? {
        if seen.contains(&id) {
            continue;
        }
        if let Some((_, m)) = load(collection, &id)? {
            if str_of(&m, "state") == state {
                out.push(m);
            }
        }
    }
    Ok(out)
}

/// `state=draft|published|archived|all` for a curator list: `all` is the whole
/// collection (nothing narrower can answer it), the rest go through [`by_state`].
pub fn in_state(collection: &str, state: &str) -> Result<Vec<Map<String, Value>>, Reply> {
    if state == "all" {
        list_all(collection)
    } else {
        by_state(collection, state)
    }
}

/// Load records by id, skipping missing ones.
pub fn load_many<'a>(
    collection: &str,
    ids: impl IntoIterator<Item = &'a String>,
) -> Result<Vec<Map<String, Value>>, Reply> {
    let mut out = Vec::new();
    for id in ids {
        if let Some((_, m)) = load(collection, id)? {
            out.push(m);
        }
    }
    Ok(out)
}

/// The index fields new journeys and quests are created with.
pub fn index_fields(fields: &[&str]) -> Vec<String> {
    fields.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: u64, id: &str) -> (u64, String) {
        (key, id.to_string())
    }

    #[test]
    fn decodes_the_query_string() {
        assert_eq!(percent_decode("golden+hour%21"), "golden hour!");
        assert_eq!(percent_decode("%E2%98%85"), "★");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(param("/api/x?view=past&q=Gold%20en", "q").as_deref(), Some("Gold en"));
        assert_eq!(param("/api/x?view=past", "q"), None);
        assert_eq!(param("/api/x", "view"), None);
    }

    #[test]
    fn a_choice_defaults_and_refuses_the_unknown() {
        let v = ["active", "past"];
        assert_eq!(choice("/c", "view", &v, "active").ok().as_deref(), Some("active"));
        assert_eq!(choice("/c?view=", "view", &v, "active").ok().as_deref(), Some("active"));
        assert_eq!(choice("/c?view=past", "view", &v, "active").ok().as_deref(), Some("past"));
        let err = choice("/c?view=later", "view", &v, "active").err().expect("refused");
        assert_eq!((err.status, err.json["error"].clone()), (400, json!("bad_view")));
    }

    #[test]
    fn search_is_a_case_insensitive_substring() {
        let q = search("/c?q=%20GOLDEN%20");
        assert_eq!(q.as_deref(), Some("golden"));
        assert!(matches("Golden hour", q.as_deref()));
        assert!(matches("the golden mile", q.as_deref()));
        assert!(!matches("Blue hour", q.as_deref()));
        assert!(matches("anything", None));
        assert_eq!(search("/c?q=%20%20"), None);
        assert_eq!(search(&format!("/c?q={}", "a".repeat(500))).map(|s| s.len()), Some(200));
    }

    #[test]
    fn limits_default_cap_and_refuse() {
        assert_eq!(page_of("/c", "s").ok().map(|p| p.limit), Some(20));
        assert_eq!(page_of("/c?limit=5", "s").ok().map(|p| p.limit), Some(5));
        assert_eq!(page_of("/c?limit=1000", "s").ok().map(|p| p.limit), Some(100));
        for bad in ["0", "-1", "ten", "1.5"] {
            let e = page_of(&format!("/c?limit={bad}"), "s").err().expect("refused");
            assert_eq!(e.json["error"], "bad_limit", "{bad}");
        }
    }

    #[test]
    fn a_cursor_round_trips_and_belongs_to_its_list() {
        let c = cursor("competitions:past", 1790000000, "01K5Y2Z7Q9ABCDEF");
        assert!(c.bytes().all(|b| b.is_ascii_hexdigit()), "opaque and URL-safe: {c}");
        assert_eq!(
            decode_cursor("competitions:past", &c),
            Some((1790000000, "01K5Y2Z7Q9ABCDEF".to_string()))
        );
        assert_eq!(decode_cursor("competitions:active", &c), None, "another view's cursor");
        assert_eq!(decode_cursor("competitions:past", "zz"), None);
        assert_eq!(decode_cursor("competitions:past", &hex("v1|competitions:past|x|a")), None);
        assert_eq!(decode_cursor("competitions:past", &hex("v1|competitions:past|1|../x")), None);
        let e = page_of(&format!("/c?after={c}"), "journeys:active").err().expect("refused");
        assert_eq!(e.json["error"], "bad_cursor");
        let p = page_of(&format!("/c?after={c}&limit=3"), "competitions:past").ok().expect("ok");
        assert_eq!(p, Page { limit: 3, after: Some((1790000000, "01K5Y2Z7Q9ABCDEF".into())) });
    }

    /// Walk every page of `items`; returns the ids in page order.
    fn walk(items: &[(u64, String)], order: Order, limit: usize) -> Vec<String> {
        let mut seen = Vec::new();
        let mut after = None;
        for _ in 0..100 {
            let page = Page { limit, after: after.clone() };
            let (got, next) = cut(items.to_vec(), |x| x.clone(), order, &page, "t");
            assert!(got.len() <= limit);
            seen.extend(got.iter().map(|x| x.1.clone()));
            match next {
                None => return seen,
                Some(c) => after = decode_cursor("t", &c),
            }
        }
        panic!("paging never ended");
    }

    #[test]
    fn pages_cover_everything_once_in_order() {
        // Equal keys on purpose: the id breaks the tie, so nothing is skipped.
        let items: Vec<(u64, String)> =
            (0..47).map(|i| item(1000 + (i / 3) as u64, &format!("id{i:03}"))).collect();
        for limit in [1, 2, 5, 20, 46, 47, 100] {
            let desc = walk(&items, Order::Desc, limit);
            assert_eq!(desc.len(), 47, "limit {limit}: every item, once");
            let unique: HashSet<&String> = desc.iter().collect();
            assert_eq!(unique.len(), 47, "limit {limit}: no duplicates");
            assert_eq!(desc.first().map(String::as_str), Some("id046"), "newest first");
            let asc = walk(&items, Order::Asc, limit);
            let mut rev = desc.clone();
            rev.reverse();
            assert_eq!(asc, rev, "limit {limit}: ascending is the reverse");
        }
    }

    #[test]
    fn the_last_page_says_so_and_an_empty_list_is_one_empty_page() {
        let items = vec![item(3, "c"), item(1, "a"), item(2, "b")];
        let (got, next) = cut(items.clone(), |x| x.clone(), Order::Desc, &Page { limit: 3, after: None }, "t");
        assert_eq!(got, vec![item(3, "c"), item(2, "b"), item(1, "a")]);
        assert_eq!(next, None, "exactly limit items: no next page");
        let (got, next) = cut(items, |x| x.clone(), Order::Desc, &Page { limit: 2, after: None }, "t");
        assert_eq!(got.len(), 2);
        assert!(next.is_some());
        let (got, next) = cut(Vec::<(u64, String)>::new(), |x| x.clone(), Order::Asc, &Page { limit: 5, after: None }, "t");
        assert!(got.is_empty() && next.is_none());
    }

    #[test]
    fn an_item_added_between_pages_causes_no_duplicate_or_gap() {
        let mut items: Vec<(u64, String)> = (0..10).map(|i| item(i, &format!("i{i:02}"))).collect();
        let (first, next) = cut(items.clone(), |x| x.clone(), Order::Desc, &Page { limit: 4, after: None }, "t");
        assert_eq!(first.last().map(|x| x.1.as_str()), Some("i06"));
        // A newer item arrives (sorts before the cursor), an old one on page 1 goes.
        items.push(item(99, "new"));
        items.retain(|x| x.1 != "i08");
        let after = decode_cursor("t", &next.expect("more"));
        let (second, _) = cut(items, |x| x.clone(), Order::Desc, &Page { limit: 4, after }, "t");
        let ids: Vec<&str> = second.iter().map(|x| x.1.as_str()).collect();
        assert_eq!(ids, ["i05", "i04", "i03", "i02"]);
    }
}
