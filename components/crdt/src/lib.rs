//! `crdt` — merge concurrent replicas with no lock — state-based CRDTs that converge whatever order updates arrive in
//!
//! State-based CvRDTs. Each value is an opaque, self-describing JSON string
//! with a `"type"` tag; `merge` and `value` dispatch on it. The invariant that
//! makes replication work: `merge` computes a least-upper-bound in a
//! join-semilattice, so it is commutative, associative, and idempotent — merge
//! order and delivery order don't matter, replicas always converge. Output is
//! canonical (serde_json with `preserve_order` off ⇒ sorted object keys, and
//! every collection here is a `BTreeMap`/`BTreeSet`), so equal merges are
//! byte-equal and equality can be checked on the string.
//!
//! Four types, one per CRDT family plus the map `scribe` builds on:
//!   - `lww`    last-writer-wins register: `(value, ts, replica)`, higher
//!     `(ts, replica)` wins.
//!   - `pn`     PN-counter: two grow-only per-replica maps (P increments, N
//!     decrements); merge = per-replica max; value = ΣP − ΣN.
//!   - `orset`  observed-remove set: per-element add-tags + a removed-tag set;
//!     an element is present iff it has an add-tag not yet removed. A remove
//!     tombstones only the tags it observed, so a concurrent add survives —
//!     add wins.
//!   - `lwwmap` per-key LWW register map (tombstones for deletes).
//!
//! Timestamps + replica ids are caller-supplied (no wall clock inside), so
//! everything is pure and deterministic. No state, no host imports.

#[allow(warnings)]
mod bindings;

use bindings::exports::crdt::merge::merger::{CrdtError, Guest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

struct Component;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum State {
    #[serde(rename = "lww")]
    Lww { v: Value, ts: u64, replica: String },
    #[serde(rename = "pn")]
    Pn { p: BTreeMap<String, u64>, n: BTreeMap<String, u64> },
    #[serde(rename = "orset")]
    OrSet { adds: BTreeMap<String, BTreeSet<String>>, removes: BTreeSet<String> },
    #[serde(rename = "lwwmap")]
    LwwMap { entries: BTreeMap<String, Reg> },
    #[serde(rename = "rga")]
    Rga { elems: BTreeMap<String, RgaElem> },
}

/// One character in an RGA sequence: its char, the id it was inserted *after*
/// (`""` = the start), and a delete tombstone. The element's own id (the map
/// key) is caller-supplied and must be globally unique + sortable — concurrent
/// inserts at the same anchor order by id (descending), so replicas agree.
#[derive(Serialize, Deserialize, Clone)]
struct RgaElem {
    ch: String,
    after: String,
    del: bool,
}

/// One LWW slot: a value (or `None` = tombstone) stamped `(ts, replica)`.
#[derive(Serialize, Deserialize, Clone)]
struct Reg {
    v: Option<Value>,
    ts: u64,
    replica: String,
}

impl Reg {
    /// Total order deciding which write wins: `(ts, replica, set?, value)`.
    /// The value is the last tiebreak so merge stays commutative even if two
    /// replicas somehow share a `(ts, replica)` stamp with different values.
    fn key(&self) -> (u64, String, u8, String) {
        (
            self.ts,
            self.replica.clone(),
            self.v.is_some() as u8,
            self.v.as_ref().map(canon).unwrap_or_default(),
        )
    }
}

// ---- helpers ------------------------------------------------------------

fn parse_state(s: &str, what: &str) -> Result<State, CrdtError> {
    let v: Value =
        serde_json::from_str(s).map_err(|e| CrdtError::InvalidJson(format!("{what}: {e}")))?;
    serde_json::from_value(v).map_err(|e| CrdtError::InvalidState(format!("{what}: {e}")))
}

fn dump(st: &State) -> Result<String, CrdtError> {
    serde_json::to_string(st).map_err(|e| CrdtError::InvalidState(format!("serialize: {e}")))
}

fn parse_val(s: &str) -> Result<Value, CrdtError> {
    serde_json::from_str(s).map_err(|e| CrdtError::InvalidJson(format!("value-json: {e}")))
}

/// Canonical (sorted-key) serialization, used only for the LWW value tiebreak.
fn canon(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Per-replica max of two grow-only counters (the G-counter join).
fn max_merge(mut a: BTreeMap<String, u64>, b: BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    for (k, v) in b {
        let e = a.entry(k).or_insert(0);
        if v > *e {
            *e = v;
        }
    }
    a
}

// ---- core join-semilattice ops (pure, binding-free, unit-tested) --------

/// The CvRDT join: least-upper-bound of two same-type states. `None` means the
/// two states are different CRDT types. Commutative, associative, idempotent.
fn merge_states(a: State, b: State) -> Option<State> {
    Some(match (a, b) {
        (State::Lww { v: va, ts: ta, replica: ra }, State::Lww { v: vb, ts: tb, replica: rb }) => {
            let ka = (ta, ra.clone(), 1u8, canon(&va));
            let kb = (tb, rb.clone(), 1u8, canon(&vb));
            if kb > ka {
                State::Lww { v: vb, ts: tb, replica: rb }
            } else {
                State::Lww { v: va, ts: ta, replica: ra }
            }
        }
        (State::Pn { p: pa, n: na }, State::Pn { p: pb, n: nb }) => {
            State::Pn { p: max_merge(pa, pb), n: max_merge(na, nb) }
        }
        (State::OrSet { adds: aa, removes: mut ra }, State::OrSet { adds: ab, removes: rb }) => {
            let mut adds = aa;
            for (el, tags) in ab {
                adds.entry(el).or_default().extend(tags);
            }
            ra.extend(rb);
            State::OrSet { adds, removes: ra }
        }
        (State::LwwMap { entries: ea }, State::LwwMap { entries: eb }) => {
            let mut entries = ea;
            for (k, rb) in eb {
                let take = match entries.get(&k) {
                    Some(ra) => rb.key() > ra.key(),
                    None => true,
                };
                if take {
                    entries.insert(k, rb);
                }
            }
            State::LwwMap { entries }
        }
        (State::Rga { elems: ea }, State::Rga { elems: eb }) => {
            // Union elements by id; a tombstone anywhere wins (delete is
            // monotonic). Both concurrent inserts are kept — that's the point.
            let mut elems = ea;
            for (id, e) in eb {
                match elems.get_mut(&id) {
                    Some(x) => x.del = x.del || e.del,
                    None => {
                        elems.insert(id, e);
                    }
                }
            }
            State::Rga { elems }
        }
        _ => return None,
    })
}

/// The sequence order of ALL elements (tombstoned included): a preorder walk of
/// the "inserted-after" tree, siblings sharing an anchor ordered by id
/// descending. Iterative (no recursion) so a long document can't overflow.
fn rga_sequence(elems: &BTreeMap<String, RgaElem>) -> Vec<String> {
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, e) in elems {
        children.entry(e.after.clone()).or_default().push(id.clone());
    }
    for v in children.values_mut() {
        v.sort(); // ascending; we push in reverse so pop yields descending
    }
    let mut out = Vec::with_capacity(elems.len());
    let mut stack: Vec<String> = children.get("").into_iter().flatten().cloned().collect();
    while let Some(id) = stack.pop() {
        out.push(id.clone());
        if let Some(kids) = children.get(&id) {
            stack.extend(kids.iter().cloned());
        }
    }
    out
}

/// The ids of the live (non-tombstoned) elements, in sequence order.
fn rga_visible(elems: &BTreeMap<String, RgaElem>) -> Vec<String> {
    rga_sequence(elems)
        .into_iter()
        .filter(|id| !elems.get(id).map(|e| e.del).unwrap_or(true))
        .collect()
}

/// The text the RGA represents.
fn rga_text(elems: &BTreeMap<String, RgaElem>) -> String {
    rga_visible(elems).iter().filter_map(|id| elems.get(id).map(|e| e.ch.as_str())).collect()
}

/// The logical value a state represents.
fn value_of(st: State) -> Value {
    match st {
        State::Lww { v, .. } => v,
        State::Pn { p, n } => {
            let sp: i128 = p.values().map(|&x| x as i128).sum();
            let sn: i128 = n.values().map(|&x| x as i128).sum();
            Value::from((sp - sn) as i64)
        }
        State::OrSet { adds, removes } => Value::Array(
            adds.into_iter()
                .filter(|(_, tags)| tags.iter().any(|t| !removes.contains(t)))
                .map(|(el, _)| Value::String(el))
                .collect(),
        ),
        State::LwwMap { entries } => {
            let mut m = serde_json::Map::new();
            for (k, r) in entries {
                if let Some(v) = r.v {
                    m.insert(k, v);
                }
            }
            Value::Object(m)
        }
        State::Rga { elems } => Value::String(rga_text(&elems)),
    }
}

// ---- guest (thin boundary: parse -> core op -> serialize) ---------------

impl Guest for Component {
    fn merge(a: String, b: String) -> Result<String, CrdtError> {
        let (a, b) = (parse_state(&a, "a")?, parse_state(&b, "b")?);
        let merged = merge_states(a, b)
            .ok_or_else(|| CrdtError::TypeMismatch("cannot merge different CRDT types".into()))?;
        dump(&merged)
    }

    fn value(state: String) -> Result<String, CrdtError> {
        let v = value_of(parse_state(&state, "state")?);
        serde_json::to_string(&v).map_err(|e| CrdtError::InvalidState(format!("serialize: {e}")))
    }

    fn lww_new(value_json: String, timestamp: u64, replica: String) -> Result<String, CrdtError> {
        dump(&State::Lww { v: parse_val(&value_json)?, ts: timestamp, replica })
    }

    fn lww_set(
        state: String,
        value_json: String,
        timestamp: u64,
        replica: String,
    ) -> Result<String, CrdtError> {
        let cand = Self::lww_new(value_json, timestamp, replica)?;
        // Same-type join with a fresh single-value register = "set if newer".
        Self::merge(state, cand)
    }

    fn counter_new() -> String {
        dump(&State::Pn { p: BTreeMap::new(), n: BTreeMap::new() }).expect("empty pn serializes")
    }

    fn counter_add(state: String, replica: String, delta: i64) -> Result<String, CrdtError> {
        let State::Pn { mut p, mut n } = parse_state(&state, "state")? else {
            return Err(CrdtError::InvalidState("expected a pn-counter".into()));
        };
        let side = if delta >= 0 { &mut p } else { &mut n };
        *side.entry(replica).or_insert(0) += delta.unsigned_abs();
        dump(&State::Pn { p, n })
    }

    fn orset_new() -> String {
        dump(&State::OrSet { adds: BTreeMap::new(), removes: BTreeSet::new() })
            .expect("empty orset serializes")
    }

    fn orset_add(state: String, element: String, tag: String) -> Result<String, CrdtError> {
        let State::OrSet { mut adds, removes } = parse_state(&state, "state")? else {
            return Err(CrdtError::InvalidState("expected an orset".into()));
        };
        adds.entry(element).or_default().insert(tag);
        dump(&State::OrSet { adds, removes })
    }

    fn orset_remove(state: String, element: String) -> Result<String, CrdtError> {
        let State::OrSet { adds, mut removes } = parse_state(&state, "state")? else {
            return Err(CrdtError::InvalidState("expected an orset".into()));
        };
        // Observed-remove: tombstone exactly the tags we currently see. Any tag
        // added concurrently (not in `adds` yet) is untouched, so it survives.
        if let Some(tags) = adds.get(&element) {
            removes.extend(tags.iter().cloned());
        }
        dump(&State::OrSet { adds, removes })
    }

    fn lwwmap_new() -> String {
        dump(&State::LwwMap { entries: BTreeMap::new() }).expect("empty lwwmap serializes")
    }

    fn lwwmap_set(
        state: String,
        key: String,
        value_json: String,
        timestamp: u64,
        replica: String,
    ) -> Result<String, CrdtError> {
        let v = Some(parse_val(&value_json)?);
        lwwmap_put(state, key, Reg { v, ts: timestamp, replica })
    }

    fn lwwmap_remove(
        state: String,
        key: String,
        timestamp: u64,
        replica: String,
    ) -> Result<String, CrdtError> {
        lwwmap_put(state, key, Reg { v: None, ts: timestamp, replica })
    }

    fn rga_new() -> String {
        dump(&State::Rga { elems: BTreeMap::new() }).expect("empty rga serializes")
    }

    fn rga_insert(
        state: String,
        index: u32,
        text: String,
        id_base: String,
    ) -> Result<String, CrdtError> {
        let State::Rga { mut elems } = parse_state(&state, "state")? else {
            return Err(CrdtError::InvalidState("expected an rga".into()));
        };
        let visible = rga_visible(&elems);
        // insert BEFORE visible[index]; anchor = the element just before it.
        let idx = (index as usize).min(visible.len());
        let mut anchor = if idx == 0 { String::new() } else { visible[idx - 1].clone() };
        // Each char becomes an element chained after the previous, so a
        // multi-char insert stays contiguous. Ids must be unique + sortable;
        // `id_base` carries the (ts, replica) order, the `:k` keeps chars apart.
        for (k, ch) in text.chars().enumerate() {
            let id = format!("{id_base}:{k:04}");
            elems.insert(
                id.clone(),
                RgaElem { ch: ch.to_string(), after: anchor.clone(), del: false },
            );
            anchor = id;
        }
        dump(&State::Rga { elems })
    }

    fn rga_delete(state: String, index: u32, count: u32) -> Result<String, CrdtError> {
        let State::Rga { mut elems } = parse_state(&state, "state")? else {
            return Err(CrdtError::InvalidState("expected an rga".into()));
        };
        let visible = rga_visible(&elems);
        let start = (index as usize).min(visible.len());
        let end = (start + count as usize).min(visible.len());
        for id in &visible[start..end] {
            if let Some(e) = elems.get_mut(id) {
                e.del = true;
            }
        }
        dump(&State::Rga { elems })
    }

    fn rga_insert_after(
        state: String,
        after_id: String,
        text: String,
        id_base: String,
    ) -> Result<String, CrdtError> {
        let State::Rga { mut elems } = parse_state(&state, "state")? else {
            return Err(CrdtError::InvalidState("expected an rga".into()));
        };
        if !after_id.is_empty() && !elems.contains_key(&after_id) {
            return Err(CrdtError::InvalidState(format!("after-id not found: {after_id}")));
        }
        let mut anchor = after_id;
        for (k, ch) in text.chars().enumerate() {
            let id = format!("{id_base}:{k:04}");
            elems.insert(
                id.clone(),
                RgaElem { ch: ch.to_string(), after: anchor.clone(), del: false },
            );
            anchor = id;
        }
        dump(&State::Rga { elems })
    }

    fn rga_delete_ids(state: String, ids: Vec<String>) -> Result<String, CrdtError> {
        let State::Rga { mut elems } = parse_state(&state, "state")? else {
            return Err(CrdtError::InvalidState("expected an rga".into()));
        };
        for id in ids {
            if let Some(e) = elems.get_mut(&id) {
                e.del = true;
            }
        }
        dump(&State::Rga { elems })
    }

    fn rga_elements(state: String) -> Result<String, CrdtError> {
        let State::Rga { elems } = parse_state(&state, "state")? else {
            return Err(CrdtError::InvalidState("expected an rga".into()));
        };
        let list: Vec<Value> = rga_visible(&elems)
            .iter()
            .filter_map(|id| elems.get(id).map(|e| json!({ "id": id, "ch": e.ch })))
            .collect();
        serde_json::to_string(&Value::Array(list))
            .map_err(|e| CrdtError::InvalidState(format!("serialize: {e}")))
    }
}

/// Apply a single register to a key iff it beats the current stamp (the
/// per-key case of the LWW-map join).
fn lwwmap_put(state: String, key: String, cand: Reg) -> Result<String, CrdtError> {
    let State::LwwMap { mut entries } = parse_state(&state, "state")? else {
        return Err(CrdtError::InvalidState("expected a lwwmap".into()));
    };
    let take = match entries.get(&key) {
        Some(cur) => cand.key() > cur.key(),
        None => true,
    };
    if take {
        entries.insert(key, cand);
    }
    dump(&State::LwwMap { entries })
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    //! Rust-side checks of the pure join-semilattice core. The convergence
    //! property is the point: any order of merges yields the same state.
    use super::*;

    fn s(json: &str) -> State {
        serde_json::from_str(json).unwrap()
    }
    fn ser(st: &State) -> String {
        serde_json::to_string(st).unwrap()
    }
    fn fold(states: &[State]) -> String {
        let mut acc = clone(&states[0]);
        for st in &states[1..] {
            acc = merge_states(acc, clone(st)).expect("same type");
        }
        ser(&acc)
    }
    fn clone(st: &State) -> State {
        s(&ser(st))
    }

    /// Every permutation of the same states folds to identical bytes
    /// (commutative + associative), and re-merging any part is a no-op
    /// (idempotent).
    fn assert_converges(states: &[State]) {
        let expected = fold(states);
        // all 6 orderings of 3 states
        let idx = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
        for order in idx {
            let permuted: Vec<State> = order.iter().map(|&i| clone(&states[i])).collect();
            assert_eq!(fold(&permuted), expected, "permutation {order:?} diverged");
        }
        let merged = s(&expected);
        assert_eq!(ser(&merge_states(clone(&merged), clone(&merged)).unwrap()), expected);
    }

    #[test]
    fn lww_higher_stamp_wins_commutatively() {
        let a = s(r#"{"type":"lww","v":"old","ts":1,"replica":"A"}"#);
        let b = s(r#"{"type":"lww","v":"new","ts":2,"replica":"B"}"#);
        assert_eq!(value_of(merge_states(clone(&a), clone(&b)).unwrap()), Value::from("new"));
        assert_eq!(
            ser(&merge_states(clone(&a), clone(&b)).unwrap()),
            ser(&merge_states(b, a).unwrap())
        );
    }

    #[test]
    fn pn_counter_sums_and_converges() {
        let a = s(r#"{"type":"pn","p":{"A":5},"n":{}}"#);
        let b = s(r#"{"type":"pn","p":{"B":4},"n":{"B":2}}"#);
        let c = s(r#"{"type":"pn","p":{},"n":{"C":1}}"#);
        assert_converges(&[clone(&a), clone(&b), clone(&c)]);
        assert_eq!(value_of(s(&fold(&[a, b, c]))), Value::from(6i64)); // 5+4-2-1
    }

    #[test]
    fn orset_add_wins_over_concurrent_remove() {
        // base: x added with tag A:1; A removes it, B re-adds with unseen tag B:1
        let removed = s(r#"{"type":"orset","adds":{"x":["A:1"]},"removes":["A:1"]}"#);
        let readded = s(r#"{"type":"orset","adds":{"x":["A:1","B:1"]},"removes":[]}"#);
        assert_eq!(
            value_of(merge_states(clone(&removed), clone(&readded)).unwrap()),
            Value::Array(vec![Value::from("x")])
        );
        assert_converges(&[
            clone(&removed),
            clone(&readded),
            s(r#"{"type":"orset","adds":{},"removes":[]}"#),
        ]);
    }

    #[test]
    fn lwwmap_converges_and_tombstone_wins() {
        let setter = s(r#"{"type":"lwwmap","entries":{"k":{"v":"v","ts":1,"replica":"A"}}}"#);
        let remover = s(r#"{"type":"lwwmap","entries":{"k":{"v":null,"ts":2,"replica":"B"}}}"#);
        let other = s(r#"{"type":"lwwmap","entries":{"j":{"v":"j","ts":1,"replica":"C"}}}"#);
        assert_converges(&[clone(&setter), clone(&remover), clone(&other)]);
        // k tombstoned (ts 2 beats 1), j survives
        assert_eq!(value_of(s(&fold(&[setter, remover, other]))), serde_json::json!({"j": "j"}));
    }

    #[test]
    fn different_types_do_not_merge() {
        let pn = s(r#"{"type":"pn","p":{},"n":{}}"#);
        let set = s(r#"{"type":"orset","adds":{},"removes":[]}"#);
        assert!(merge_states(pn, set).is_none());
    }

    // ---- RGA (text sequence) --------------------------------------------
    fn text(state: &str) -> String {
        match value_of(s(state)) {
            Value::String(t) => t,
            v => panic!("not a string value: {v}"),
        }
    }
    fn ins(state: String, index: u32, t: &str, id: &str) -> String {
        Component::rga_insert(state, index, t.into(), id.into()).unwrap()
    }

    #[test]
    fn rga_builds_and_edits_text() {
        let a = ins(Component::rga_new(), 0, "hello", "0001-a");
        assert_eq!(text(&a), "hello");
        let a = ins(a, 5, " world", "0002-a"); // append
        assert_eq!(text(&a), "hello world");
        let a = ins(a, 0, "say ", "0003-a"); // prepend
        assert_eq!(text(&a), "say hello world");
        let a = Component::rga_delete(a, 0, 4).unwrap(); // drop "say "
        assert_eq!(text(&a), "hello world");
    }

    /// The headline: two replicas insert at the SAME position concurrently.
    /// Both characters survive (neither is lost, unlike LWW), and every replica
    /// orders them identically — deterministic interleaving.
    #[test]
    fn rga_concurrent_inserts_both_survive_and_converge() {
        let base = ins(Component::rga_new(), 0, "AC", "0000-seed");
        // both insert between A and C, from different replicas
        let rx = ins(base.clone(), 1, "X", "0001-x");
        let ry = ins(base.clone(), 1, "Y", "0002-y");
        // higher id sorts first among same-anchor siblings -> Y before X
        assert_eq!(text(&Component::merge(rx.clone(), ry.clone()).unwrap()), "AYXC");
        // commutative: merge order doesn't matter
        assert_eq!(
            Component::merge(rx.clone(), ry.clone()).unwrap(),
            Component::merge(ry.clone(), rx.clone()).unwrap()
        );
        assert_converges(&[s(&rx), s(&ry), s(&base)]);
    }

    /// Id-anchored ops: what a real editor sends. A concurrent insert elsewhere
    /// can't shift where this one lands, because it anchors to a stable id.
    #[test]
    fn rga_id_anchored_ops_are_unambiguous() {
        let base = ins(Component::rga_new(), 0, "AC", "0000-seed");
        // find the ids of A and C
        let elems: Value =
            serde_json::from_str(&Component::rga_elements(base.clone()).unwrap()).unwrap();
        let arr = elems.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let id_a = arr[0]["id"].as_str().unwrap().to_string();
        assert_eq!(arr[0]["ch"], "A");
        // two replicas insert AFTER A concurrently — both survive, deterministic
        let rx =
            Component::rga_insert_after(base.clone(), id_a.clone(), "X".into(), "0001-x".into())
                .unwrap();
        let ry =
            Component::rga_insert_after(base.clone(), id_a.clone(), "Y".into(), "0002-y".into())
                .unwrap();
        assert_eq!(text(&Component::merge(rx.clone(), ry.clone()).unwrap()), "AYXC");
        // delete by id
        let del = Component::rga_delete_ids(base.clone(), vec![id_a]).unwrap();
        assert_eq!(text(&del), "C");
        // inserting after an unknown id is rejected
        assert!(Component::rga_insert_after(base, "nope".into(), "z".into(), "9-z".into()).is_err());
    }

    #[test]
    fn rga_delete_and_concurrent_edit_both_apply() {
        let base = ins(Component::rga_new(), 0, "abc", "0000-s");
        let deleted = Component::rga_delete(base.clone(), 1, 1).unwrap(); // "ac"
        let edited = ins(base.clone(), 3, "d", "0001-e"); // "abcd"
                                                          // merge: b stays deleted, d survives -> "acd"
        assert_eq!(text(&Component::merge(deleted.clone(), edited.clone()).unwrap()), "acd");
        assert_converges(&[s(&deleted), s(&edited), s(&base)]);
    }
}
