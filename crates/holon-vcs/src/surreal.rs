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
//! | `op_effect` | `sha256(ws, op)` | the conflict state changes one op made |
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
//! # Atomicity
//!
//! Each method is one request; the multi-statement ones run in a `BEGIN …
//! COMMIT` transaction. The conditional writes the engine relies on —
//! `put_patch` replacing only a pending record, `abandon_if_uncommitted` touching
//! only `opened_at = 0` — are single `UPDATE … WHERE` / `IF` statements inside
//! one transaction, so they are atomic on the server.

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
/// table is defined up front.
const SCHEMA: &str = "
DEFINE TABLE IF NOT EXISTS symbol SCHEMALESS;
DEFINE TABLE IF NOT EXISTS patch SCHEMALESS;
DEFINE TABLE IF NOT EXISTS symref SCHEMALESS;
DEFINE TABLE IF NOT EXISTS conflict SCHEMALESS;
DEFINE TABLE IF NOT EXISTS op_effect SCHEMALESS;
DEFINE TABLE IF NOT EXISTS depends_on TYPE RELATION SCHEMALESS;
DEFINE TABLE IF NOT EXISTS implements TYPE RELATION SCHEMALESS;
DEFINE TABLE IF NOT EXISTS conflicts_with TYPE RELATION SCHEMALESS;
DEFINE INDEX IF NOT EXISTS symbol_component ON symbol FIELDS ws, component;
DEFINE INDEX IF NOT EXISTS conflict_key ON conflict FIELDS ws, key, state;
DEFINE INDEX IF NOT EXISTS conflict_ws ON conflict FIELDS ws, state;
";

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
            match self.db.query(sql).bind(vars.clone()).await.and_then(|r| r.check()) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    let msg = e.to_string();
                    if !is_retryable(&msg) {
                        return Err(VcsError::Storage(format!("surreal: {msg}")));
                    }
                    last = msg;
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(std::time::Duration::from_millis(50));
                }
            }
        }
        Err(VcsError::Storage(format!(
            "surreal: gave up after {TXN_TRIES} transaction conflicts: {last}"
        )))
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
            "UPDATE type::record('symbol', $rid) SET
                name = $name,
                aliases = array::union(aliases, [$name]),
                tip = $tip,
                deleted = $deleted,
                wit_binding = $wit,
                position = position ?? $pos;",
            json!({
                "rid": rid(ws, key),
                "name": u.name,
                "tip": u.tip,
                "deleted": u.deleted,
                "wit": u.wit_binding,
                "pos": u.position_if_unset,
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
        op: Option<OpId>,
    ) -> Result<()> {
        self.run(
            "UPDATE type::record('patch', $rid) SET status = $status, op = $op ?? op;",
            json!({ "rid": rid(ws, hash), "status": to_value(&status)?, "op": op }),
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

    async fn put_conflict(&self, ws: &str, c: &ConflictRecord) -> Result<()> {
        self.run(
            "BEGIN;
             UPSERT type::record('conflict', $rid) CONTENT $row;
             DELETE conflicts_with WHERE ws = $ws AND conflict = $cid;
             RELATE (type::record('patch', $left))->conflicts_with->(type::record('patch', $right))
                 SET ws = $ws, conflict = $cid;
             COMMIT;",
            json!({
                "rid": rid(ws, &c.id),
                "row": conflict_row(ws, c)?,
                "ws": ws,
                "cid": c.id,
                "left": rid(ws, &c.left.patch),
                "right": rid(ws, &c.right.patch),
            }),
        )
        .await?;
        Ok(())
    }

    async fn abandon_if_uncommitted(&self, ws: &str, id: &str) -> Result<()> {
        self.run(
            "UPDATE type::record('conflict', $rid) SET state = 'abandoned' WHERE opened_at = 0;",
            json!({ "rid": rid(ws, id) }),
        )
        .await?;
        Ok(())
    }

    async fn commit_conflict(&self, ws: &str, id: &str, op: OpId) -> Result<()> {
        self.run(
            "UPDATE type::record('conflict', $rid) SET state = 'open', opened_at = $op;",
            json!({ "rid": rid(ws, id), "op": op }),
        )
        .await?;
        Ok(())
    }

    async fn set_conflict_state(
        &self,
        ws: &str,
        id: &str,
        state: ConflictState,
        resolved_by: Option<Hash>,
    ) -> Result<()> {
        self.run(
            "UPDATE type::record('conflict', $rid) SET state = $state, resolved_by = $rb;",
            json!({ "rid": rid(ws, id), "state": state.as_str(), "rb": resolved_by }),
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

    async fn put_effects(&self, ws: &str, op: OpId, effects: &[ConflictEffect]) -> Result<()> {
        self.run(
            "UPSERT type::record('op_effect', $rid) CONTENT { ws: $ws, op: $op, effects: $effects };",
            json!({ "rid": rid(ws, &op.to_string()), "ws": ws, "op": op, "effects": to_value(&effects)? }),
        )
        .await?;
        Ok(())
    }

    async fn effects(&self, ws: &str, op: OpId) -> Result<Vec<ConflictEffect>> {
        #[derive(serde::Deserialize)]
        struct Row {
            effects: Vec<ConflictEffect>,
        }
        // Not `SELECT VALUE effects`: taking an `Option` of an array result takes
        // its first element.
        let v: Option<Row> = self
            .one(
                "SELECT effects FROM ONLY type::record('op_effect', $rid);",
                json!({ "rid": rid(ws, &op.to_string()) }),
                0,
            )
            .await?;
        Ok(v.map(|r| r.effects).unwrap_or_default())
    }
}
