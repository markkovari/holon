//! The [`Graph`] on SurrealDB (`native` feature), over the 3.x WebSocket client.
//!
//! # Schema
//!
//! | table | record id | holds |
//! |---|---|---|
//! | `symbol` | `sha256(ws, key)` | a [`SymbolRecord`] + `ws` |
//! | `patch` | `sha256(ws, hash)` | a [`PatchRecord`] + `ws` |
//! | `symref` | `sha256(ws, symbol id)` | a symbol *name* an edge can point at |
//! | `conflict` | `sha256(ws, id)` | a [`ConflictRecord`] + `ws`, its `id` stored as `cid` |
//!
//! (Conflict effects live in the oplog's intents, not here.)
//!
//! Edges, all `RELATE`d: `patch ->depends_on-> symref`, `patch ->implements->
//! symref` (a patch's content uses / implements those names — by name, because
//! that is what source refers by; after a rename the old name's dependents are
//! exactly the code still using the old name), and `patch ->conflicts_with->
//! patch` (left to right, carrying the conflict id). `dependents` is a graph
//! traversal: `symref<-depends_on<-patch`.
//!
//! Record ids are hashes, so no user string is ever spliced into a query: every
//! value — workspace, names, content hashes — is a bound parameter, and ids are
//! built server-side with `type::record(<table>, $rid)`. The only identifiers
//! interpolated are the namespace and database at connect time, which are checked
//! against `[A-Za-z0-9_]` first.
//!
//! # Indexes
//!
//! Every lookup the adapter makes is either by record id (a patch by hash, a
//! symbol or conflict by key, a symref) or through an index, so its cost is
//! bounded by what it returns, not by how much any workspace has written. Record
//! ids are hashes of `(ws, natural id)`, so different workspaces never share a
//! record; every other lookup leads with `ws`.
//!
//! | lookup | statement | index |
//! |---|---|---|
//! | symbols by name (`symbols_named`) | `symbol WHERE ws, component, path, kind` (+ alias filter) | `symbol_name` |
//! | symbols of a component | `symbol WHERE ws, component` | `symbol_component` |
//! | open conflicts of a symbol | `conflict WHERE ws, key, state` | `conflict_key` |
//! | conflicts of a workspace (by state) | `conflict WHERE ws [, state]` | `conflict_ws` |
//! | a patch's old edges, on rewrite | `DELETE depends_on / implements WHERE in` | `depends_on_in`, `implements_in` |
//! | a conflict's edge, on rewrite | `DELETE conflicts_with WHERE ws, conflict` | `conflicts_with_cid` |
//! | dependents | `symref<-depends_on<-patch` | graph edge scan (no index needed) |
//!
//! `tests/live.rs::surreal_queries_use_indexes` runs `EXPLAIN` on each and
//! fails on a table scan; `tests/bench.rs` measures them on 20k patches.
//!
//! # Atomicity
//!
//! Each method is one request; the multi-statement ones run in a `BEGIN …
//! COMMIT` transaction. The conditional writes the engine relies on —
//! `put_patch` replacing only a pending record, the op-monotone guards on
//! `mark_patch`, `update_symbol` and conflict effects, `abandon_if_uncommitted`
//! touching only an uncommitted conflict — are `UPDATE … WHERE` / `IF`
//! statements inside one transaction, so they are atomic on the server.

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use surrealdb::engine::remote::ws::{Client, Ws};
use surrealdb::opt::auth::Root;
use surrealdb::Surreal;

use crate::error::{Result, VcsError};
use crate::graph::{
    ConflictEffect, ConflictRecord, Graph, PatchRecord, PatchStatus, SymbolRecord, SymbolUpdate,
};
use crate::model::{ConflictState, Hash, OpId, SymbolId};
use crate::store::sha256_hex;

#[derive(Debug, Clone)]
pub struct SurrealConfig {
    /// `host:port`, as the WebSocket engine takes it (`127.0.0.1:8000`).
    pub address: String,
    pub username: String,
    pub password: String,
    pub namespace: String,
    pub database: String,
}

impl SurrealConfig {
    pub fn new(address: &str) -> Self {
        SurrealConfig {
            address: address.trim_start_matches("ws://").trim_end_matches('/').to_string(),
            username: "root".into(),
            password: "root".into(),
            namespace: "holon".into(),
            database: "vcs".into(),
        }
    }
}

pub struct SurrealGraph {
    db: Surreal<Client>,
}

fn ident(s: &str) -> Result<&str> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(VcsError::Invalid(format!("{s:?} is not a SurrealDB identifier")));
    }
    Ok(s)
}

/// A record id: a hash of the workspace and the natural id, so it is always a
/// plain hex string whatever the user typed.
fn rid(ws: &str, id: &str) -> String {
    let mut b = Vec::with_capacity(ws.len() + id.len() + 1);
    b.extend_from_slice(ws.as_bytes());
    b.push(0);
    b.extend_from_slice(id.as_bytes());
    sha256_hex(&b)
}

fn symbol_ref(ws: &str, id: &SymbolId) -> String {
    rid(ws, &format!("{}\0{}\0{}\0{}", id.component, id.path, id.kind.as_str(), id.name))
}

fn to_value<T: serde::Serialize>(t: &T) -> Result<Value> {
    serde_json::to_value(t).map_err(VcsError::storage)
}

fn with_ws<T: serde::Serialize>(ws: &str, t: &T) -> Result<Value> {
    let mut v = to_value(t)?;
    if let Value::Object(m) = &mut v {
        m.insert("ws".into(), Value::String(ws.to_string()));
    }
    Ok(v)
}

/// A conflict's own `id` would collide with the record id, so it is stored as
/// `cid`.
fn conflict_row(ws: &str, c: &ConflictRecord) -> Result<Value> {
    let mut v = with_ws(ws, c)?;
    if let Value::Object(m) = &mut v {
        if let Some(id) = m.remove("id") {
            m.insert("cid".into(), id);
        }
    }
    Ok(v)
}

fn conflict_from(mut v: Value) -> Result<ConflictRecord> {
    if let Value::Object(m) = &mut v {
        if let Some(id) = m.remove("cid") {
            m.insert("id".into(), id);
        }
    }
    from_value(v)
}

fn from_value<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| VcsError::Storage(format!("surreal row: {e}")))
}

/// SurrealDB 3 errors on a SELECT from a table that does not exist, so every
/// table is defined up front — with an index for every lookup (module docs).
pub const SCHEMA: &str = "
DEFINE TABLE IF NOT EXISTS symbol SCHEMALESS;
DEFINE TABLE IF NOT EXISTS patch SCHEMALESS;
DEFINE TABLE IF NOT EXISTS symref SCHEMALESS;
DEFINE TABLE IF NOT EXISTS conflict SCHEMALESS;
DEFINE TABLE IF NOT EXISTS depends_on TYPE RELATION SCHEMALESS;
DEFINE TABLE IF NOT EXISTS implements TYPE RELATION SCHEMALESS;
DEFINE TABLE IF NOT EXISTS conflicts_with TYPE RELATION SCHEMALESS;
DEFINE INDEX IF NOT EXISTS symbol_name ON symbol FIELDS ws, component, path, kind;
DEFINE INDEX IF NOT EXISTS symbol_component ON symbol FIELDS ws, component;
DEFINE INDEX IF NOT EXISTS conflict_key ON conflict FIELDS ws, key, state;
DEFINE INDEX IF NOT EXISTS conflict_ws ON conflict FIELDS ws, state;
DEFINE INDEX IF NOT EXISTS depends_on_in ON depends_on FIELDS in;
DEFINE INDEX IF NOT EXISTS implements_in ON implements FIELDS in;
DEFINE INDEX IF NOT EXISTS conflicts_with_cid ON conflicts_with FIELDS ws, conflict;
";

/// Every read and delete-by-condition the adapter issues, with sample
/// parameters, for the `EXPLAIN` check in the live suite: `(name, statement)`.
pub const INDEXED_QUERIES: &[(&str, &str)] = &[
    (
        "symbols_named",
        "SELECT VALUE key FROM symbol WHERE ws = 'w' AND component = 'c' AND path = 'p' AND kind = 'function' AND aliases CONTAINS 'n'",
    ),
    ("symbols_in_component", "SELECT * FROM symbol WHERE ws = 'w' AND component = 'c'"),
    ("open_conflicts_for", "SELECT * FROM conflict WHERE ws = 'w' AND key = 'k' AND state = 'open'"),
    ("conflicts(state)", "SELECT * FROM conflict WHERE ws = 'w' AND state = 'open'"),
    ("conflicts", "SELECT * FROM conflict WHERE ws = 'w'"),
    ("put_patch: depends_on", "DELETE depends_on WHERE in = patch:x"),
    ("put_patch: implements", "DELETE implements WHERE in = patch:x"),
    ("put_conflict: conflicts_with", "DELETE conflicts_with WHERE ws = 'w' AND conflict = 'c'"),
];

impl SurrealGraph {
    pub async fn connect(cfg: &SurrealConfig) -> Result<Self> {
        let ns = ident(&cfg.namespace)?;
        let dbn = ident(&cfg.database)?;
        let db = Surreal::new::<Ws>(cfg.address.as_str()).await.map_err(VcsError::storage)?;
        db.signin(Root { username: cfg.username.clone(), password: cfg.password.clone() })
            .await
            .map_err(VcsError::storage)?;
        let g = SurrealGraph { db };
        g.run(
            &format!("DEFINE NAMESPACE IF NOT EXISTS {ns}; USE NS {ns}; DEFINE DATABASE IF NOT EXISTS {dbn};"),
            json!({}),
        )
        .await?;
        g.db.use_ns(ns).use_db(dbn).await.map_err(VcsError::storage)?;
        g.run(SCHEMA, json!({})).await?;
        Ok(g)
    }

    /// Run `sql` with `vars`; every statement must succeed.
    ///
    /// SurrealDB's transactions are optimistic: two that write the same key
    /// concurrently and one is cancelled with a "can be retried" conflict, having
    /// changed nothing. That is retried here (bounded) — every statement this
    /// adapter sends is either a single atomic statement or one `BEGIN … COMMIT`
    /// block, so a cancelled one can simply run again.
    async fn run(&self, sql: &str, vars: Value) -> Result<surrealdb::IndexedResults> {
        let mut delay = std::time::Duration::from_millis(1);
        let mut last = String::new();
        for _ in 0..TXN_TRIES {
            let mut r = match self.db.query(sql).bind(vars.clone()).await {
                Ok(r) => r,
                Err(e) => {
                    let msg = e.to_string();
                    if !is_retryable(&msg) {
                        return Err(VcsError::Storage(format!("surreal: {msg}")));
                    }
                    last = msg;
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(std::time::Duration::from_millis(50));
                    continue;
                }
            };
            let mut errors: Vec<(usize, String)> =
                r.take_errors().into_iter().map(|(i, e)| (i, e.to_string())).collect();
            if errors.is_empty() {
                return Ok(r);
            }
            errors.sort();
            // In a `BEGIN … COMMIT` block every statement reports the failed
            // transaction; the cause is whichever says something else. None
            // saying anything else still means nothing was applied.
            let causes: Vec<&str> = errors
                .iter()
                .map(|(_, m)| m.as_str())
                .filter(|m| !m.contains("failed transaction"))
                .collect();
            if !causes.is_empty() && !causes.iter().any(|m| is_retryable(m)) {
                return Err(VcsError::Storage(format!("surreal: {}", causes.join("; "))));
            }
            last = errors.into_iter().map(|(_, m)| m).collect::<Vec<_>>().join("; ");
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(std::time::Duration::from_millis(50));
        }
        Err(VcsError::Storage(format!(
            "surreal: gave up after {TXN_TRIES} transaction conflicts: {last}"
        )))
    }

    /// The plan of `sql` (a SELECT or DELETE, without the trailing `;`), as
    /// SurrealDB's `EXPLAIN` renders it.
    pub async fn explain(&self, sql: &str) -> Result<Value> {
        // A SELECT's plan is one object, a DELETE's a list of steps: wrap both.
        let mut r = self.run(&format!("RETURN [({sql} EXPLAIN)];"), json!({})).await?;
        let v: Vec<Value> = r.take(0).map_err(VcsError::storage)?;
        Ok(Value::Array(v))
    }

    /// Drop this graph's database (tests: one database per run).
    pub async fn remove_database(&self, database: &str) -> Result<()> {
        let dbn = ident(database)?;
        self.run(&format!("REMOVE DATABASE IF EXISTS {dbn};"), json!({})).await?;
        Ok(())
    }

    /// Rows of statement `idx`.
    async fn rows<T: DeserializeOwned>(
        &self,
        sql: &str,
        vars: Value,
        idx: usize,
    ) -> Result<Vec<T>> {
        let mut r = self.run(sql, vars).await?;
        let v: Vec<Value> = r.take(idx).map_err(VcsError::storage)?;
        v.into_iter().map(from_value).collect()
    }

    async fn one<T: DeserializeOwned>(
        &self,
        sql: &str,
        vars: Value,
        idx: usize,
    ) -> Result<Option<T>> {
        let mut r = self.run(sql, vars).await?;
        let v: Option<Value> = r.take(idx).map_err(VcsError::storage)?;
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(v) => from_value(v).map(Some),
        }
    }
}

/// Transaction-conflict retries per request.
pub const TXN_TRIES: u32 = 100;

fn is_retryable(msg: &str) -> bool {
    msg.contains("can be retried")
        || msg.contains("Transaction conflict")
        || msg.contains("Write conflict")
}

impl Graph for SurrealGraph {
    async fn ensure_symbol(&self, ws: &str, key: &str, id: &SymbolId) -> Result<()> {
        let rec = SymbolRecord {
            key: key.to_string(),
            component: id.component.clone(),
            path: id.path.clone(),
            kind: id.kind,
            name: id.name.clone(),
            aliases: vec![],
            tip: None,
            deleted: true,
            position: None,
            wit_binding: None,
            last_op: 0,
        };
        let mut row = with_ws(ws, &rec)?;
        row["id"] = Value::String(rid(ws, key));
        self.run(
            "BEGIN;
             INSERT IGNORE INTO symbol $row;
             UPDATE type::record('symbol', $rid) SET aliases = array::union(aliases, [$name]);
             COMMIT;",
            json!({ "row": row, "rid": rid(ws, key), "name": id.name }),
        )
        .await?;
        Ok(())
    }

    async fn update_symbol(&self, ws: &str, key: &str, u: SymbolUpdate) -> Result<()> {
        self.run(
            "BEGIN;
             LET $r = type::record('symbol', $rid);
             UPDATE $r SET aliases = array::union(aliases, [$name]), position = position ?? $pos;
             UPDATE $r SET
                name = $name,
                tip = $tip,
                deleted = $deleted,
                wit_binding = $wit,
                last_op = $op
             WHERE (last_op ?? 0) <= $op;
             COMMIT;",
            json!({
                "rid": rid(ws, key),
                "name": u.name,
                "tip": u.tip,
                "deleted": u.deleted,
                "wit": u.wit_binding,
                "pos": u.position_if_unset,
                "op": u.op,
            }),
        )
        .await?;
        Ok(())
    }

    async fn symbol(&self, ws: &str, key: &str) -> Result<Option<SymbolRecord>> {
        self.one(
            "SELECT * OMIT id, ws FROM ONLY type::record('symbol', $rid);",
            json!({ "rid": rid(ws, key) }),
            0,
        )
        .await
    }

    async fn symbols_named(&self, ws: &str, id: &SymbolId) -> Result<Vec<String>> {
        let mut keys: Vec<String> = self
            .rows(
                "SELECT VALUE key FROM symbol
                 WHERE ws = $ws AND component = $c AND path = $p AND kind = $k AND aliases CONTAINS $n;",
                json!({ "ws": ws, "c": id.component, "p": id.path, "k": id.kind.as_str(), "n": id.name }),
                0,
            )
            .await?;
        keys.sort();
        Ok(keys)
    }

    async fn symbols_in_component(&self, ws: &str, component: &str) -> Result<Vec<SymbolRecord>> {
        self.rows(
            "SELECT * OMIT id, ws FROM symbol WHERE ws = $ws AND component = $c;",
            json!({ "ws": ws, "c": component }),
            0,
        )
        .await
    }

    async fn put_patch(&self, ws: &str, p: &PatchRecord) -> Result<()> {
        let edges = |ids: &[SymbolId]| -> Result<Vec<Value>> {
            ids.iter()
                .map(|s| Ok(json!({ "rid": symbol_ref(ws, s), "id": to_value(s)? })))
                .collect()
        };
        self.run(
            "BEGIN;
             LET $p = type::record('patch', $rid);
             LET $cur = (SELECT VALUE status.status FROM ONLY $p);
             IF $cur = NONE OR $cur = 'pending' {
                 UPSERT $p CONTENT $row;
                 DELETE depends_on WHERE in = $p;
                 DELETE implements WHERE in = $p;
                 FOR $t IN $deps {
                     UPSERT type::record('symref', $t.rid) CONTENT { ws: $ws, symbol: $t.id };
                     RELATE $p->depends_on->(type::record('symref', $t.rid)) SET ws = $ws;
                 };
                 FOR $t IN $impls {
                     UPSERT type::record('symref', $t.rid) CONTENT { ws: $ws, symbol: $t.id };
                     RELATE $p->implements->(type::record('symref', $t.rid)) SET ws = $ws;
                 };
             };
             COMMIT;",
            json!({
                "rid": rid(ws, &p.hash),
                "row": with_ws(ws, p)?,
                "ws": ws,
                "deps": edges(&p.depends_on)?,
                "impls": edges(&p.implements)?,
            }),
        )
        .await?;
        Ok(())
    }

    async fn patch(&self, ws: &str, hash: &str) -> Result<Option<PatchRecord>> {
        self.one(
            "SELECT * OMIT id, ws FROM ONLY type::record('patch', $rid);",
            json!({ "rid": rid(ws, hash) }),
            0,
        )
        .await
    }

    async fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: OpId,
        set_op: bool,
    ) -> Result<()> {
        self.run(
            "UPDATE type::record('patch', $rid)
                SET status = $status, status_op = $op, op = $setop ?? op
                WHERE (status_op ?? 0) <= $op;",
            json!({
                "rid": rid(ws, hash),
                "status": to_value(&status)?,
                "op": op,
                "setop": set_op.then_some(op),
            }),
        )
        .await?;
        Ok(())
    }

    async fn dependents(&self, ws: &str, target: &SymbolId) -> Result<Vec<Hash>> {
        let mut out: Vec<Hash> = self
            .rows(
                "LET $s = type::record('symref', $rid);
                 SELECT VALUE hash FROM $s<-depends_on<-patch;",
                json!({ "rid": symbol_ref(ws, target) }),
                1,
            )
            .await?;
        out.sort();
        out.dedup();
        Ok(out)
    }

    async fn put_conflict(&self, ws: &str, c: &ConflictRecord, op: OpId) -> Result<()> {
        let mut fresh = c.clone();
        fresh.state = ConflictState::Open;
        fresh.opened_at = 0;
        fresh.resolved_by = None;
        fresh.pending = vec![op];
        fresh.state_op = 0;
        self.run(
            "BEGIN;
             LET $r = type::record('conflict', $rid);
             LET $cur = (SELECT state, opened_at, state_op FROM ONLY $r);
             IF $cur = NONE OR ($cur.state = 'abandoned' AND $op > ($cur.state_op ?? 0)) {
                 UPSERT $r CONTENT $row;
                 UPDATE $r SET state_op = $cur.state_op ?? 0;
                 DELETE conflicts_with WHERE ws = $ws AND conflict = $cid;
                 RELATE (type::record('patch', $left))->conflicts_with->(type::record('patch', $right))
                     SET ws = $ws, conflict = $cid;
             } ELSE IF $cur.state = 'open' AND $cur.opened_at = 0 {
                 UPDATE $r SET pending = array::union(pending ?? [], [$op]);
             };
             COMMIT;",
            json!({
                "rid": rid(ws, &c.id),
                "row": conflict_row(ws, &fresh)?,
                "ws": ws,
                "cid": c.id,
                "op": op,
                "left": rid(ws, &c.left.patch),
                "right": rid(ws, &c.right.patch),
            }),
        )
        .await?;
        Ok(())
    }

    async fn abandon_if_uncommitted(&self, ws: &str, id: &str, op: OpId) -> Result<()> {
        self.run(
            "BEGIN;
             LET $r = type::record('conflict', $rid);
             UPDATE $r SET pending = array::complement(pending ?? [], [$op]);
             UPDATE $r SET state = 'abandoned'
                 WHERE opened_at = 0 AND state = 'open' AND array::len(pending ?? []) = 0;
             COMMIT;",
            json!({ "rid": rid(ws, id), "op": op }),
        )
        .await?;
        Ok(())
    }

    async fn apply_conflict_effect(&self, ws: &str, e: &ConflictEffect, op: OpId) -> Result<()> {
        self.run(
            "BEGIN;
             LET $r = type::record('conflict', $rid);
             UPDATE $r SET opened_at = $op
                 WHERE (state_op ?? 0) <= $op AND $after = 'open' AND opened_at = 0;
             UPDATE $r SET
                 state = $after,
                 resolved_by = $rb,
                 state_op = $op,
                 pending = array::complement(pending ?? [], [$op])
             WHERE (state_op ?? 0) <= $op;
             COMMIT;",
            json!({
                "rid": rid(ws, &e.conflict),
                "op": op,
                "after": e.after.as_str(),
                "rb": e.resolved_by_after,
            }),
        )
        .await?;
        Ok(())
    }

    async fn conflict(&self, ws: &str, id: &str) -> Result<Option<ConflictRecord>> {
        let v: Option<Value> = self
            .one(
                "SELECT * OMIT id, ws FROM ONLY type::record('conflict', $rid);",
                json!({ "rid": rid(ws, id) }),
                0,
            )
            .await?;
        v.map(conflict_from).transpose()
    }

    async fn conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> Result<Vec<ConflictRecord>> {
        let rows: Vec<Value> = match state {
            Some(s) => {
                self.rows(
                    "SELECT * OMIT id, ws FROM conflict WHERE ws = $ws AND state = $state;",
                    json!({ "ws": ws, "state": s.as_str() }),
                    0,
                )
                .await
            }
            None => {
                self.rows(
                    "SELECT * OMIT id, ws FROM conflict WHERE ws = $ws;",
                    json!({ "ws": ws }),
                    0,
                )
                .await
            }
        }?;
        rows.into_iter().map(conflict_from).collect()
    }

    async fn open_conflicts_for(&self, ws: &str, key: &str) -> Result<Vec<ConflictRecord>> {
        let rows: Vec<Value> = self
            .rows(
                "SELECT * OMIT id, ws FROM conflict WHERE ws = $ws AND key = $key AND state = 'open';",
                json!({ "ws": ws, "key": key }),
                0,
            )
            .await?;
        rows.into_iter().map(conflict_from).collect()
    }
}
