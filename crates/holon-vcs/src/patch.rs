//! The pure decision core: what a patch is called, and what happens to it.
//!
//! Nothing here does I/O. The engine reads a symbol's state, hands it to
//! [`decide`], and carries out the [`Decision`] with a compare-and-set; when the
//! CAS loses, it reads again and decides again.
//!
//! # Names
//!
//! * A **symbol key** is the stable identity a symbol's tip pointer is filed
//!   under: [`symbol_key`]`(id, n)`, the SHA-256 of the symbol id it was first
//!   created as and a probe index `n`. Deterministic, so two agents racing to
//!   create the same symbol race on the same pointer. A rename keeps the key; if a
//!   name is later reused by a new symbol, that one takes the next free `n`.
//! * A **patch hash** is the SHA-256 of [`canonical`]`(key, sorted parents,
//!   change)`. Content is always a blob hash in the change (inline text is stored
//!   first), so `inline("x")` and `blob(sha256("x"))` are the same patch. The
//!   agent, message and time are metadata, NOT hashed: the identical change by two
//!   agents is one patch, and the second is a `duplicate`.
//! * A **conflict id** is the SHA-256 of its two side hashes, sorted.
//! * A **name key** ([`name_key`]) is the SHA-256 of a full symbol id under its
//!   own domain tag: the pointer `ws/<w>/name/<name key>` reserves that id for
//!   the one symbol key holding it (see the engine's notes on names).
//! * A `create`'s placement, and a `move`'s, are part of the change the hash
//!   covers — as REQUESTED (`after(x)`), not as resolved to an order key, so a
//!   retry of a placed create is a duplicate even if the file changed since.
//!
//! # The decision
//!
//! Let `tip` be the symbol's current tip patch (none if the key was never
//! written or its create was reverted), `live` whether it exists and is not a
//! delete, `h` the hash of the request.
//!
//! | request | condition | decision |
//! |---|---|---|
//! | `create`, parent given | | `invalid` |
//! | `replace`/`delete`/`rename`, no parent | | `invalid` |
//! | `create` | not `live` | `applied` (parents: `[tip]` if the tip is a delete, else `[]`) — unless open conflicts: `unresolved-conflict` |
//! | `create` | `live` | the *stale* rule, base `none` |
//! | other | no tip at all | `symbol-not-found` |
//! | other | `h == tip` | `duplicate` |
//! | other | `parent == tip`, not `live` | `symbol-not-found` (edit of a deleted symbol) |
//! | other | `parent == tip`, open conflicts | `unresolved-conflict` |
//! | other | `parent == tip` | `applied` |
//! | other | `parent != tip` | the *stale* rule, base `parent` |
//!
//! The **stale** rule (the tip moved on THIS symbol since the parent): if `h` is
//! already recorded — landed, or the right side of a conflict that is open or
//! resolved — it is the same edit again: `duplicate` (or the same `conflicted`
//! answer, if its conflict is still open). Otherwise it is `conflicted`:
//! `left` = the current tip, `right` = `h`, `base` = the parent. The tip does not
//! move. A stale edit is recorded as a conflict even while other conflicts on
//! the symbol are open: every loser of a race is kept, each paired with the tip it
//! lost to. Only an edit that would MOVE the tip (parent == tip) is refused while
//! conflicts are open, since it would build on a state somebody disputes.

use sha2::{Digest, Sha256};

use crate::error::{Result, VcsError};
use crate::graph::{Change, ConflictRecord, PatchRecord, PatchStatus};
use crate::model::{ConflictId, ConflictState, Hash, Placement, SymbolId};

/// The stable key a symbol created as `id` is filed under, at probe index `n`.
pub fn symbol_key(id: &SymbolId, n: u32) -> String {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"holon-vcs/symbol/v1\n");
    field(&mut buf, &id.component);
    field(&mut buf, &id.path);
    field(&mut buf, id.kind.as_str());
    field(&mut buf, &id.name);
    field(&mut buf, &n.to_string());
    hex::encode(Sha256::digest(&buf))
}

/// The key of the name pointer that reserves `id`.
pub fn name_key(id: &SymbolId) -> String {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"holon-vcs/name/v1\n");
    symbol_fields(&mut buf, id);
    hex::encode(Sha256::digest(&buf))
}

/// Whether `key` is where a symbol CREATED as `id` would be filed (any probe).
pub fn is_probe_key(id: &SymbolId, key: &str, max_probe: u32) -> bool {
    (0..max_probe).any(|n| symbol_key(id, n) == key)
}

fn symbol_fields(buf: &mut Vec<u8>, id: &SymbolId) {
    field(buf, &id.component);
    field(buf, &id.path);
    field(buf, id.kind.as_str());
    field(buf, &id.name);
}

fn placement_fields(buf: &mut Vec<u8>, p: &Placement) {
    match p {
        Placement::First => field(buf, "first"),
        Placement::Last => field(buf, "last"),
        Placement::After(id) => {
            field(buf, "after");
            symbol_fields(buf, id);
        }
        Placement::Before(id) => {
            field(buf, "before");
            symbol_fields(buf, id);
        }
    }
}

/// Length-prefixed, so no field's content can shift a boundary.
fn field(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.len().to_string().as_bytes());
    buf.push(b':');
    buf.extend_from_slice(s.as_bytes());
    buf.push(b'\n');
}

/// The canonical encoding a patch hash is taken over. `at` is a `create`'s
/// placement; nothing is added when it is `None`, so unplaced patches hash as
/// they did in step two.
pub fn canonical(key: &str, parents: &[Hash], change: &Change, at: Option<&Placement>) -> Vec<u8> {
    let mut sorted = parents.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut buf = Vec::new();
    buf.extend_from_slice(b"holon-vcs/patch/v1\n");
    field(&mut buf, key);
    field(&mut buf, &sorted.len().to_string());
    for p in &sorted {
        field(&mut buf, p);
    }
    match change {
        Change::Create(h) => {
            field(&mut buf, "create");
            field(&mut buf, h);
        }
        Change::Replace(h) => {
            field(&mut buf, "replace");
            field(&mut buf, h);
        }
        Change::Delete => field(&mut buf, "delete"),
        Change::Rename(n) => {
            field(&mut buf, "rename");
            field(&mut buf, n);
        }
        Change::Move(p) => {
            field(&mut buf, "move");
            placement_fields(&mut buf, p);
        }
    }
    if let Some(p) = at {
        field(&mut buf, "at");
        placement_fields(&mut buf, p);
    }
    buf
}

pub fn patch_hash(key: &str, parents: &[Hash], change: &Change, at: Option<&Placement>) -> Hash {
    hex::encode(Sha256::digest(canonical(key, parents, change, at)))
}

/// SHA-256 of the two side hashes, sorted — the same collision found twice (or
/// from either side) is one conflict.
pub fn conflict_id(a: &str, b: &str) -> ConflictId {
    let (x, y) = if a <= b { (a, b) } else { (b, a) };
    hex::encode(Sha256::digest(format!("holon-vcs/conflict/v1\n{x}\n{y}\n").as_bytes()))
}

/// Everything [`decide`] looks at.
pub struct Inputs<'a> {
    pub key: &'a str,
    pub symbol: &'a SymbolId,
    pub parent: Option<&'a Hash>,
    pub change: &'a Change,
    /// A `create`'s placement.
    pub placement: Option<&'a Placement>,
    /// The symbol's tip patch, `None` if there is no tip.
    pub tip: Option<&'a PatchRecord>,
    /// The graph's record of the request's hash (computed from `parent`), if any.
    pub recorded: Option<&'a PatchRecord>,
    /// The graph's record of `parent`, when it is not the tip.
    pub parent_record: Option<&'a PatchRecord>,
    /// Open conflicts on this symbol.
    pub open: &'a [ConflictRecord],
    /// All conflicts `recorded` might be the right side of (open or not); only
    /// consulted for the stale rule.
    pub recorded_conflict: Option<&'a ConflictRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Move the tip to `patch`.
    Applied { patch: Hash, parents: Vec<Hash> },
    /// Already there; write nothing.
    Duplicate { patch: Hash },
    /// Record a conflict `left` (the tip) vs `patch`; the tip does not move.
    Conflicted { patch: Hash, parents: Vec<Hash>, base: Option<Hash>, left: Hash },
    /// The same stale edit again, whose conflict is still open.
    AlreadyConflicted { patch: Hash, conflict: ConflictId },
}

/// The hash a request would have, given its parent (not the resurrection case:
/// see [`decide`]).
pub fn request_hash(
    key: &str,
    parent: Option<&Hash>,
    change: &Change,
    at: Option<&Placement>,
) -> Hash {
    let parents: Vec<Hash> = parent.into_iter().cloned().collect();
    patch_hash(key, &parents, change, at)
}

pub fn decide(i: &Inputs<'_>) -> Result<Decision> {
    let is_create = matches!(i.change, Change::Create(_));
    match (is_create, i.parent) {
        (true, Some(_)) => {
            return Err(VcsError::Invalid("`create` must not name a parent".into()));
        }
        (false, None) => {
            return Err(VcsError::Invalid(
                "`replace`, `delete` and `rename` must name the parent they were computed from"
                    .into(),
            ));
        }
        _ => {}
    }
    let live = i.tip.is_some_and(|t| !t.is_delete());
    let open_ids = || -> Vec<ConflictId> { i.open.iter().map(|c| c.id.clone()).collect() };

    if is_create {
        if !live {
            if !i.open.is_empty() {
                return Err(VcsError::UnresolvedConflict(open_ids()));
            }
            // A dead symbol's tip is its delete patch: build on it, so recreating
            // the same content is a new patch rather than the old create again.
            let parents: Vec<Hash> = i.tip.map(|t| vec![t.hash.clone()]).unwrap_or_default();
            let patch = patch_hash(i.key, &parents, i.change, i.placement);
            return Ok(Decision::Applied { patch, parents });
        }
        let tip = i.tip.expect("live implies a tip");
        let h = request_hash(i.key, None, i.change, i.placement);
        if h == tip.hash {
            return Ok(Decision::Duplicate { patch: h });
        }
        return Ok(stale(i, h, vec![], None, tip));
    }

    let parent = i.parent.expect("checked above");
    let Some(tip) = i.tip else {
        return Err(VcsError::SymbolNotFound(i.symbol.clone()));
    };
    let h = request_hash(i.key, Some(parent), i.change, None);
    if h == tip.hash {
        return Ok(Decision::Duplicate { patch: h });
    }
    if *parent == tip.hash {
        if !live {
            return Err(VcsError::SymbolNotFound(i.symbol.clone()));
        }
        if !i.open.is_empty() {
            return Err(VcsError::UnresolvedConflict(open_ids()));
        }
        return Ok(Decision::Applied { patch: h, parents: vec![parent.clone()] });
    }
    match i.parent_record {
        None => return Err(VcsError::NotFound(format!("parent patch {parent}"))),
        Some(p) if p.key != i.key => {
            return Err(VcsError::Invalid(format!(
                "parent {parent} is a patch of another symbol ({})",
                p.symbol
            )));
        }
        Some(_) => {}
    }
    Ok(stale(i, h, vec![parent.clone()], Some(parent.clone()), tip))
}

fn stale(
    i: &Inputs<'_>,
    h: Hash,
    parents: Vec<Hash>,
    base: Option<Hash>,
    tip: &PatchRecord,
) -> Decision {
    if let Some(rec) = i.recorded {
        match &rec.status {
            PatchStatus::Landed => return Decision::Duplicate { patch: h },
            PatchStatus::Conflicted(cid) => {
                if let Some(c) = i.recorded_conflict.filter(|c| &c.id == cid) {
                    match c.state {
                        ConflictState::Open => {
                            return Decision::AlreadyConflicted { patch: h, conflict: cid.clone() };
                        }
                        ConflictState::Resolved => return Decision::Duplicate { patch: h },
                        // Abandoned (its opening was reverted): it is a live
                        // disagreement again, so record it again.
                        ConflictState::Abandoned => {}
                    }
                }
            }
            PatchStatus::Pending => {}
        }
    }
    Decision::Conflicted { patch: h, parents, base, left: tip.hash.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::SideRecord;
    use crate::model::{Agent, SymbolKind};

    fn sym() -> SymbolId {
        SymbolId::new("orders", "src/lib.rs", "compute_total", SymbolKind::Function)
    }

    fn rec(key: &str, hash: &str, content: Option<&str>) -> PatchRecord {
        PatchRecord {
            hash: hash.into(),
            key: key.into(),
            symbol: sym(),
            parents: vec![],
            change: match content {
                Some(c) => Change::Replace(c.into()),
                None => Change::Delete,
            },
            content: content.map(Into::into),
            agent: Agent::named("a"),
            message: None,
            at: 0,
            op: Some(1),
            status: PatchStatus::Landed,
            depends_on: vec![],
            implements: vec![],
            wit_binding: None,
            status_op: 1,
            order: None,
            placement: None,
        }
    }

    fn inputs<'a>(
        key: &'a str,
        s: &'a SymbolId,
        parent: Option<&'a Hash>,
        change: &'a Change,
        tip: Option<&'a PatchRecord>,
    ) -> Inputs<'a> {
        Inputs {
            key,
            symbol: s,
            parent,
            change,
            placement: None,
            tip,
            recorded: None,
            parent_record: None,
            open: &[],
            recorded_conflict: None,
        }
    }

    #[test]
    fn hash_ignores_parent_order_and_metadata() {
        let c = Change::Replace("b".repeat(64));
        let a = patch_hash("k", &["1".into(), "2".into()], &c, None);
        let b = patch_hash("k", &["2".into(), "1".into()], &c, None);
        assert_eq!(a, b);
        assert_ne!(a, patch_hash("k2", &["1".into(), "2".into()], &c, None));
        assert_ne!(a, patch_hash("k", &["1".into()], &c, None));
        // A placement is part of what is hashed; `first` and `last` differ.
        let first = patch_hash("k", &[], &c, Some(&Placement::First));
        assert_ne!(first, patch_hash("k", &[], &c, None));
        assert_ne!(first, patch_hash("k", &[], &c, Some(&Placement::Last)));
        assert_ne!(
            patch_hash("k", &[], &c, Some(&Placement::After(sym()))),
            patch_hash("k", &[], &c, Some(&Placement::Before(sym())))
        );
    }

    #[test]
    fn keys_are_deterministic_and_probe_distinct() {
        assert_eq!(symbol_key(&sym(), 0), symbol_key(&sym(), 0));
        assert_ne!(symbol_key(&sym(), 0), symbol_key(&sym(), 1));
        assert_ne!(symbol_key(&sym(), 0), symbol_key(&sym().renamed("x"), 0));
        assert_ne!(name_key(&sym()), symbol_key(&sym(), 0));
        assert_ne!(name_key(&sym()), name_key(&sym().renamed("x")));
        assert!(is_probe_key(&sym(), &symbol_key(&sym(), 3), 64));
        assert!(!is_probe_key(&sym(), &symbol_key(&sym().renamed("x"), 0), 64));
    }

    #[test]
    fn conflict_id_is_symmetric() {
        assert_eq!(conflict_id("a", "b"), conflict_id("b", "a"));
        assert_ne!(conflict_id("a", "b"), conflict_id("a", "c"));
    }

    #[test]
    fn validation() {
        let s = sym();
        let p = "a".repeat(64);
        let create = Change::Create("c".repeat(64));
        let replace = Change::Replace("c".repeat(64));
        assert!(matches!(
            decide(&inputs("k", &s, Some(&p), &create, None)),
            Err(VcsError::Invalid(_))
        ));
        assert!(matches!(
            decide(&inputs("k", &s, None, &replace, None)),
            Err(VcsError::Invalid(_))
        ));
        assert!(matches!(
            decide(&inputs("k", &s, Some(&p), &Change::Delete, None)),
            Err(VcsError::SymbolNotFound(_))
        ));
    }

    #[test]
    fn applied_duplicate_conflicted() {
        let s = sym();
        let base = rec("k", &"a".repeat(64), Some(&"0".repeat(64)));
        let change = Change::Replace("1".repeat(64));
        // parent == tip
        let d = decide(&inputs("k", &s, Some(&base.hash), &change, Some(&base))).unwrap();
        let Decision::Applied { patch, parents } = d else { panic!("{d:?}") };
        assert_eq!(parents, vec![base.hash.clone()]);
        // that patch is now the tip: the same request is a duplicate
        let mut landed = rec("k", &patch, Some(&"1".repeat(64)));
        landed.parents = parents;
        let d = decide(&inputs("k", &s, Some(&base.hash), &change, Some(&landed))).unwrap();
        assert_eq!(d, Decision::Duplicate { patch: patch.clone() });
        // a different edit from the same parent is a conflict against the tip
        let other = Change::Replace("2".repeat(64));
        let mut i = inputs("k", &s, Some(&base.hash), &other, Some(&landed));
        i.parent_record = Some(&base);
        let d = decide(&i).unwrap();
        let Decision::Conflicted { left, base: b, .. } = d else { panic!("{d:?}") };
        assert_eq!(left, patch);
        assert_eq!(b, Some(base.hash.clone()));
    }

    #[test]
    fn open_conflict_blocks_only_forward_edits() {
        let s = sym();
        let tip = rec("k", &"a".repeat(64), Some(&"0".repeat(64)));
        let old = rec("k", &"b".repeat(64), Some(&"9".repeat(64)));
        let open = [ConflictRecord {
            id: "c".into(),
            key: "k".into(),
            symbol: s.clone(),
            base: None,
            left: SideRecord { patch: tip.hash.clone(), agent: Agent::named("a"), content: None },
            right: SideRecord { patch: "d".repeat(64), agent: Agent::named("b"), content: None },
            state: ConflictState::Open,
            opened_at: 2,
            resolved_by: None,
            pending: vec![],
            state_op: 2,
        }];
        let change = Change::Replace("1".repeat(64));
        let mut i = inputs("k", &s, Some(&tip.hash), &change, Some(&tip));
        i.open = &open;
        assert!(
            matches!(decide(&i), Err(VcsError::UnresolvedConflict(ids)) if ids == vec!["c".to_string()])
        );
        // a stale edit is still recorded — as another conflict
        let mut i = inputs("k", &s, Some(&old.hash), &change, Some(&tip));
        i.open = &open;
        i.parent_record = Some(&old);
        assert!(matches!(decide(&i), Ok(Decision::Conflicted { .. })));
    }

    #[test]
    fn recreate_after_delete_builds_on_the_delete() {
        let s = sym();
        let del = rec("k", &"d".repeat(64), None);
        let change = Change::Create("1".repeat(64));
        let d = decide(&inputs("k", &s, None, &change, Some(&del))).unwrap();
        let Decision::Applied { parents, patch } = d else { panic!() };
        assert_eq!(parents, vec![del.hash.clone()]);
        assert_ne!(patch, request_hash("k", None, &change, None));
        // and editing the deleted symbol is not-found
        let r = Change::Replace("1".repeat(64));
        assert!(matches!(
            decide(&inputs("k", &s, Some(&del.hash), &r, Some(&del))),
            Err(VcsError::SymbolNotFound(_))
        ));
    }
}
