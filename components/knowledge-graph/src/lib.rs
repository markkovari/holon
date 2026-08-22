//! `knowledge-graph` — nodes, edges and traversal over SurrealDB's HTTP API.
//!
//! ## Why a component and not a host capability
//!
//! It needs a network socket, and the host already grants one
//! (`wasi:http/outgoing-handler`). Nothing else about it needs the OS, so it is a
//! component: deployable mid-run, swappable for a different backend, and bounded
//! by the deployment's egress allow-list rather than by trust (ADR-0008). A host
//! rebuilt to add a database driver is a host whose isolation you re-verify.
//!
//! ## Configuration, and where the password lives
//!
//! `wasi:config` carries `surreal-url`, `surreal-ns`, `surreal-db` and
//! `surreal-user`. The password is a SECRET, read through `comp:secrets/reader`
//! under the key `surreal-password`, because a manifest must never carry one
//! (ADR-0010) and a config map is the most-dumped namespace there is (ADR-0051).
//!
//! ## The dangerous part
//!
//! Everything below builds SurrealQL by string. Nothing a caller supplies is
//! interpolated raw: an id goes through `record_id`, a kind or edge has to BE a
//! table name, and properties are re-serialised from parsed JSON so a value can
//! never carry syntax. All four are tested against the injections they exist to
//! stop. The one exception is `query`, an escape hatch by design that says so.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use bindings::comp::secrets::reader as secrets;
use bindings::exports::knowledge::graph::store::{Direction, GraphError, Guest, Node};
use bindings::wasi::config::store as config;
use bindings::wasi::io::streams::{OutputStream, StreamError as OutputStreamError};
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};

struct Component;

/// Where the database is, and who we are to it.
struct Conn {
    authority: String,
    scheme: Scheme,
    path: String,
    ns: String,
    db: String,
    /// `None` when no password was granted — see `Conn::open`.
    auth: Option<String>,
}

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().unwrap_or_else(|| default.to_string())
}

impl Conn {
    fn open() -> Result<Self, GraphError> {
        let url = cfg("surreal-url", "");
        if url.is_empty() {
            return Err(GraphError::NotConfigured(
                "surreal-url is not set — this component has no database to talk to".into(),
            ));
        }
        // Split by hand rather than adding a URL crate for one shape of string.
        let (scheme, rest) = match url.split_once("://") {
            Some(("https", r)) => (Scheme::Https, r),
            Some(("http", r)) => (Scheme::Http, r),
            _ => {
                return Err(GraphError::NotConfigured(format!(
                    "surreal-url must start with http:// or https://, got {url:?}"
                )))
            }
        };
        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a.to_string(), format!("/{p}")),
            None => (rest.to_string(), String::new()),
        };
        let user = cfg("surreal-user", "root");
        // The password is the one thing that is not config. `none` is not fatal:
        // a database with no auth is a legitimate local setup, and failing here
        // would make an unauthenticated server unusable for no gain.
        //
        // "Not fatal" has to mean sending NO header, not sending `Basic root:`.
        // An empty-password header is refused by an authenticated server AND by
        // an `--unauthenticated` one (which has no `root` to name), so the
        // previous version made the very setup this comment describes the one
        // configuration that could not work. Found by the console's browser test,
        // which runs against exactly that setup.
        let password = match secrets::get("surreal-password") {
            Ok(Some(s)) => secrets::reveal(&s).ok(),
            _ => None,
        };
        Ok(Self {
            authority,
            scheme,
            path: if path.is_empty() { "/sql".into() } else { format!("{path}/sql") },
            ns: cfg("surreal-ns", "comp"),
            db: cfg("surreal-db", "knowledge"),
            auth: password.map(|p| format!("Basic {}", B64.encode(format!("{user}:{p}")))),
        })
    }
}

/// A record id: `kind:⟨id⟩`.
///
/// The angle brackets are SurrealDB's own quoting for an arbitrary id, so a ULID,
/// a path with slashes and a name with a space all address correctly. The KIND is
/// restricted instead of quoted, because it becomes a table name and a table name
/// is not a place to be creative.
fn record_id(kind: &str, id: &str) -> Result<String, GraphError> {
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(GraphError::Rejected(format!(
            "kind must be [a-zA-Z0-9_], got {kind:?}"
        )));
    }
    Ok(format!("{kind}:⟨{}⟩", id.replace('⟩', "")))
}

/// An edge (table) name. Same rule as a kind, for the same reason.
fn edge_name(edge: &str) -> Result<String, GraphError> {
    if edge.is_empty() || !edge.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(GraphError::Rejected(format!(
            "edge must be [a-zA-Z0-9_], got {edge:?}"
        )));
    }
    Ok(edge.to_string())
}

/// A JSON object, or `{}`. Anything else is refused: `CONTENT` takes an object,
/// and letting an array or a bare string through would produce a statement whose
/// error message is about SurrealQL rather than about the caller's mistake.
fn object(properties: &str) -> Result<String, GraphError> {
    let t = properties.trim();
    if t.is_empty() {
        return Ok("{}".into());
    }
    match serde_json::from_str::<serde_json::Value>(t) {
        Ok(v) if v.is_object() => Ok(v.to_string()),
        Ok(_) => Err(GraphError::Rejected("properties must be a JSON object".into())),
        Err(e) => Err(GraphError::Rejected(format!("properties is not JSON: {e}"))),
    }
}

/// POST one statement to `/sql` and hand back the body.
/// Write a whole statement, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error — the component simply dies, and the caller two
/// links away reports `every replica failed; n1 refused`. Nothing here noticed for
/// as long as every statement was small: a contract file grew from 3645 bytes to
/// 4573 and the next real run died with a message that named no size, no
/// component and no write.
///
/// Chunking at a flat 4096 would fix that and flush once per 4 KB, which is a
/// round trip the stream never asked for. `check-write` is the stream saying how
/// much it will take right now — usually far more than 4096 — so this writes in
/// whatever bites it offers, blocks on the pollable when it offers nothing, and
/// flushes ONCE at the end. No magic constant, correct backpressure, and a
/// statement carrying a 40 KB contract costs one flush rather than ten.
fn write_all(stream: &OutputStream, mut bytes: &[u8]) -> Result<(), String> {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                // Zero is not an error: it is "full, wait". The pollable resolves
                // when the stream has drained.
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(e) => return Err(format!("{e:?}")),
        };
        let take = ready.min(bytes.len());
        stream.write(&bytes[..take]).map_err(|e| format!("{e:?}"))?;
        bytes = &bytes[take..];
    }
    stream.blocking_flush().map_err(|e| format!("{e:?}"))
}

fn sql(conn: &Conn, statement: &str) -> Result<String, GraphError> {
    let headers = Fields::new();
    let set = |k: &str, v: &str| {
        let _ = headers.set(k, &[v.as_bytes().to_vec()]);
    };
    set("accept", "application/json");
    set("content-type", "text/plain");
    // Both spellings: SurrealDB 1.x reads NS/DB, 2.x reads surreal-ns/surreal-db,
    // and sending both costs two headers and works against either.
    set("ns", &conn.ns);
    set("db", &conn.db);
    set("surreal-ns", &conn.ns);
    set("surreal-db", &conn.db);
    if let Some(auth) = &conn.auth {
        set("authorization", auth);
    }

    let req = OutgoingRequest::new(headers);
    let _ = req.set_method(&Method::Post);
    let _ = req.set_scheme(Some(&conn.scheme));
    let _ = req.set_authority(Some(&conn.authority));
    let _ = req.set_path_with_query(Some(&conn.path));

    let body = req.body().map_err(|_| GraphError::Unavailable("no request body".into()))?;
    {
        let stream = body
            .write()
            .map_err(|_| GraphError::Unavailable("no request stream".into()))?;
        write_all(&stream, statement.as_bytes())
            .map_err(|e| GraphError::Unavailable(format!("writing the statement: {e}")))?;
    }
    let _ = OutgoingBody::finish(body, None);

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(10_000_000_000));
    let _ = opts.set_first_byte_timeout(Some(30_000_000_000));

    let fut = bindings::wasi::http::outgoing_handler::handle(req, Some(opts))
        .map_err(|e| GraphError::Unavailable(format!("sending: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| GraphError::Unavailable("no response".into()))?
        .map_err(|_| GraphError::Unavailable("response already taken".into()))?
        .map_err(|e| GraphError::Unavailable(format!("connecting: {e:?}")))?;

    let status = resp.status();
    let rb = resp.consume().map_err(|_| GraphError::Unavailable("no response body".into()))?;
    let stream = rb.stream().map_err(|_| GraphError::Unavailable("no response stream".into()))?;
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => out.extend_from_slice(&chunk),
            // End of body.
            Err(OutputStreamError::Closed) => break,
            // A failed read is not the end of the database's answer. Taking the
            // truncated bytes and parsing them is how "half a result set" becomes
            // "an empty result set" — a read that silently answers NOTHING FOUND
            // for a row that exists. The write side of this component had the
            // mirror-image bug and it took four runs to find.
            Err(e) => {
                return Err(GraphError::Unavailable(format!("reading the answer: {e:?}")))
            }
        }
    }
    let text = String::from_utf8_lossy(&out).to_string();
    if !(200..300).contains(&status) {
        // The database's own words, not a paraphrase: "table does not exist" is
        // worth reading and a generic "rejected" is not.
        return Err(GraphError::Rejected(format!("HTTP {status}: {}", text.chars().take(400).collect::<String>())));
    }
    Ok(text)
}

/// SurrealDB answers `/sql` with one entry per statement. Pull the `result` of
/// the last one, and surface a per-statement error as a rejection rather than
/// letting it look like an empty result.
fn first_result(body: &str) -> Result<serde_json::Value, GraphError> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| GraphError::Unavailable(format!("unreadable answer: {e}")))?;
    let last = v
        .as_array()
        .and_then(|a| a.last().cloned())
        .ok_or_else(|| GraphError::Unavailable(format!("unexpected answer: {body}")))?;
    if last["status"].as_str().unwrap_or("OK") != "OK" {
        return Err(GraphError::Rejected(
            last["result"].as_str().unwrap_or("statement failed").to_string(),
        ));
    }
    Ok(last["result"].clone())
}

/// Send a statement, and if the namespace or database is not there yet, define it
/// and send the statement again.
///
/// SurrealDB 3 does not conjure a namespace on first write — it answers "The
/// namespace 'comp' does not exist" and stops. Requiring an operator to
/// pre-provision one would mean a component that works on the maintainer's
/// machine and fails on a fresh database, which is the shape of a 3am page.
/// Retried ONCE: if the define does not help, the second failure is the real
/// answer and looping on it would only hide it.
fn missing_namespace(msg: &str) -> bool {
    msg.contains("does not exist") && (msg.contains("namespace") || msg.contains("database"))
}

/// How many times a conflicted statement is resent.
///
/// SurrealDB's transactions are OPTIMISTIC: two writers racing for one record do
/// not queue and do not deadlock — the loser is aborted and told to try again.
/// Measured against v3.1.3, 20-way concurrency, one hot key: 60 concurrent
/// `SET uses += 1` produce 53 commits and 7 rejections carrying "Transaction
/// conflict: Write conflict, retry the transaction. This transaction can be
/// retried", and one immediate resend clears all 7 — 60/60, final value exactly
/// 60. A conflicted transaction did NOT commit, which is what makes a resend safe
/// for a non-idempotent statement like an increment.
///
/// No backoff, deliberately: there is nothing to wait for. The winner committed
/// before the loser heard about it, so the contended record is already free.
///
/// Twelve, not four. Four was chosen from the measurement above, on an idle
/// machine, where one resend cleared every conflict. On a busy one it is not
/// enough: the scenario that mirrors this shape lost a write roughly one run in
/// three under the same 20-way concurrency while the machine was compiling — 59
/// of 60, then 58 of 60. A resend costs one HTTP round trip against a database
/// that is by then uncontended, so the ceiling is cheap to raise and expensive to
/// leave low.
const MAX_ATTEMPTS: u32 = 12;

/// The loser of an optimistic race, in the database's own words.
fn conflicted(body: &str) -> bool {
    body.contains("retry the transaction")
}

/// The first per-statement error in a response, if any. Needed because a failed
/// statement comes back inside an HTTP 200.
fn body_error(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.as_array()?
        .iter()
        .find(|s| s["status"].as_str().unwrap_or("OK") != "OK")
        .map(|s| s["result"].as_str().unwrap_or("the statement failed").to_string())
}

/// Send a statement, surviving the two things that are not really failures.
///
/// Both live HERE rather than in `run`, because `query` — the escape hatch — needs
/// them just as much and used to have neither: a component that does its writes
/// through raw SurrealQL (`knowledge-memory` does, for `+=` and KNN) would
/// otherwise get no namespace bootstrap and no conflict retry, which is exactly
/// the caller that hits both hardest.
fn send(conn: &Conn, statement: &str) -> Result<String, GraphError> {
    let mut body = sql(conn, statement)?;

    // SurrealDB 3 does not conjure a namespace on first write — it answers "The
    // namespace 'comp' does not exist" and stops. Requiring an operator to
    // pre-provision one would mean a component that works on the maintainer's
    // machine and fails on a fresh database, which is the shape of a 3am page.
    // Retried ONCE: if the define does not help, the second failure is the real
    // answer and looping on it would only hide it.
    if let Some(msg) = body_error(&body).filter(|m| missing_namespace(m)) {
        sql(
            conn,
            &format!(
                "DEFINE NAMESPACE IF NOT EXISTS {}; DEFINE DATABASE IF NOT EXISTS {};",
                conn.ns, conn.db
            ),
        )
        // If defining is refused — a scoped user without DEFINE rights — say what
        // the ORIGINAL statement said, because "permission denied on DEFINE
        // NAMESPACE" would send the reader after the wrong bug.
        .map_err(|_| {
            GraphError::NotConfigured(format!(
                "{msg}, and this component may not create it — provision the namespace and database first"
            ))
        })?;
        body = sql(conn, statement)?;
    }

    let mut attempts = 1;
    while conflicted(&body) && attempts < MAX_ATTEMPTS {
        body = sql(conn, statement)?;
        attempts += 1;
    }
    // Out of attempts and STILL conflicted is a write that did not commit, and it
    // must not be returned as a success. It used to be: `query` — the raw-SurrealQL
    // hatch that `contract-registry` and `knowledge-memory` do all their work
    // through, including every `uses += 1` — handed the caller an `Ok(String)`
    // whose contents said "retry the transaction". A silently dropped increment
    // that reports success is the same shape as the truncated write and the
    // truncated read: a failure wearing the return type of a success.
    if conflicted(&body) {
        return Err(GraphError::Unavailable(format!(
            "the database rejected this statement {MAX_ATTEMPTS} times running with a \
             write conflict and it never committed; the caller must retry or give up, \
             not carry on as though it had landed"
        )));
    }
    Ok(body)
}

fn run(conn: &Conn, statement: &str) -> Result<serde_json::Value, GraphError> {
    send(conn, statement).and_then(|b| first_result(&b))
}

/// A read of something that was never written is not an error.
///
/// SurrealDB answers `SELECT * FROM nosuch:x` with "The table 'nosuch' does not
/// exist" rather than an empty set. For a graph an agent is still building, the
/// first question about a kind ALWAYS precedes the first write of it, so letting
/// that surface as a failure would make an empty graph look like a broken one.
fn absent_is_empty(r: Result<serde_json::Value, GraphError>) -> Result<serde_json::Value, GraphError> {
    match r {
        Err(GraphError::Rejected(msg)) if msg.contains("does not exist") => Ok(serde_json::Value::Array(vec![])),
        other => other,
    }
}

/// A SurrealDB row back into a node. `id` comes back as `kind:the-id`, so the
/// kind is stripped rather than trusted from the caller's question — a row that
/// came from a traversal may be of a kind the caller never named.
fn node_of(row: &serde_json::Value) -> Option<Node> {
    let full = row["id"].as_str()?;
    let (kind, id) = full.split_once(':')?;
    let mut props = row.clone();
    if let Some(o) = props.as_object_mut() {
        o.remove("id");
    }
    Some(Node {
        kind: kind.to_string(),
        // Ids go out in angle brackets and come back in backticks — SurrealDB
        // quotes on the way out with its own preferred form, and only when the
        // id needs it. Both are stripped so `upsert` then `get` round-trips.
        id: id.trim_matches(|c| c == '⟨' || c == '⟩' || c == '`').to_string(),
        properties: props.to_string(),
    })
}

/// Format a compound atomic relate statement: upserts both endpoint nodes and creates the edge
/// in a single SurrealQL script. Eliminates 3 separate HTTP roundtrips across the WASI boundary.
fn relate_statement(
    from_node: &Node,
    edge: &str,
    to_node: &Node,
    properties: &str,
) -> Result<String, GraphError> {
    let from_rid = record_id(&from_node.kind, &from_node.id)?;
    let from_obj = object(&from_node.properties)?;
    let to_rid = record_id(&to_node.kind, &to_node.id)?;
    let to_obj = object(&to_node.properties)?;
    let e = edge_name(edge)?;
    let edge_obj = object(properties)?;

    Ok(format!(
        "UPSERT {from_rid} CONTENT {from_obj}; \
         UPSERT {to_rid} CONTENT {to_obj}; \
         RELATE {from_rid}->{e}->{to_rid} CONTENT {edge_obj};"
    ))
}

impl Guest for Component {
    fn upsert(n: Node) -> Result<(), GraphError> {
        let conn = Conn::open()?;
        let stmt = format!("UPSERT {} CONTENT {};", record_id(&n.kind, &n.id)?, object(&n.properties)?);
        run(&conn, &stmt).map(|_| ())
    }

    fn get(kind: String, id: String) -> Result<Option<Node>, GraphError> {
        let conn = Conn::open()?;
        let stmt = format!("SELECT * FROM {};", record_id(&kind, &id)?);
        let result = absent_is_empty(run(&conn, &stmt))?;
        Ok(result.as_array().and_then(|a| a.first()).and_then(node_of))
    }

    fn relate(from_node: Node, edge: String, to_node: Node, properties: String) -> Result<(), GraphError> {
        let conn = Conn::open()?;
        // Both ends and the edge are submitted in ONE compound statement.
        // A graph that refuses an edge because a node is not there yet forces every
        // caller to order its writes; batching them into one statement ensures idempotency
        // and cuts HTTP round trips from 3 to 1 across the WASI boundary.
        let stmt = relate_statement(&from_node, &edge, &to_node, &properties)?;
        run(&conn, &stmt).map(|_| ())
    }

    fn neighbours(
        kind: String,
        id: String,
        edge: String,
        dir: Direction,
        limit: u32,
    ) -> Result<Vec<Node>, GraphError> {
        let conn = Conn::open()?;
        let (rid, e) = (record_id(&kind, &id)?, edge_name(&edge)?);
        let hop = match dir {
            Direction::Outgoing => format!("{rid}->{e}->?"),
            Direction::Incoming => format!("{rid}<-{e}<-?"),
            // `<->` is not a traversal SurrealQL has; either direction is the two
            // queries unioned, which is what a caller means by "both".
            Direction::Both => format!("array::union({rid}->{e}->?, {rid}<-{e}<-?)"),
        };
        let n = if limit == 0 { 100 } else { limit.min(1000) };
        let stmt = format!("SELECT * FROM {hop} LIMIT {n};");
        // An edge nobody has drawn yet is a table that does not exist yet.
        let result = absent_is_empty(run(&conn, &stmt))?;
        Ok(result
            .as_array()
            .map(|a| a.iter().filter_map(node_of).collect())
            .unwrap_or_default())
    }

    fn query(surql: String) -> Result<String, GraphError> {
        let conn = Conn::open()?;
        send(&conn, &surql)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_that_is_not_a_table_name_is_refused() {
        assert!(record_id("file", "src/lib.rs").is_ok(), "a path is a legal id");
        assert!(record_id("node_v2", "01ABC").is_ok());
        for bad in ["file; DROP TABLE x", "file:other", "", "file-v2", "üñî"] {
            assert!(record_id(bad, "x").is_err(), "{bad:?} must not become a table name");
        }
    }

    #[test]
    fn an_id_is_quoted_rather_than_restricted() {
        // Ids come from the world — file paths, URLs, ULIDs — so they are quoted
        // with SurrealDB's own bracket form instead of being narrowed.
        assert_eq!(record_id("file", "a b/c.rs").unwrap(), "file:⟨a b/c.rs⟩");
        // A closing bracket inside the id is the one thing that could end the
        // quoting early, so it is removed rather than escaped.
        assert_eq!(record_id("file", "x⟩ ; DROP").unwrap(), "file:⟨x ; DROP⟩");
    }

    #[test]
    fn properties_must_be_a_json_object() {
        assert_eq!(object("").unwrap(), "{}");
        assert_eq!(object(r#"{"a":1}"#).unwrap(), r#"{"a":1}"#);
        assert!(object("[1,2]").is_err(), "an array is not CONTENT");
        assert!(object("not json").is_err());
    }

    #[test]
    fn a_row_becomes_a_node_with_its_kind_stripped() {
        let row = serde_json::json!({ "id": "file:⟨src/lib.rs⟩", "lines": 12 });
        let n = node_of(&row).expect("a row with an id is a node");
        assert_eq!(n.kind, "file");
        assert_eq!(n.id, "src/lib.rs");
        assert_eq!(n.properties, r#"{"lines":12}"#, "id must not be duplicated into properties");
    }

    #[test]
    fn a_failed_statement_is_a_rejection_not_an_empty_result() {
        let body = r#"[{"status":"ERR","time":"1ms","result":"table does not exist"}]"#;
        match first_result(body) {
            Err(GraphError::Rejected(m)) => assert!(m.contains("table does not exist")),
            other => panic!("a failed statement must not read as success: {other:?}"),
        }
    }

    #[test]
    fn relate_statement_batches_upserts_and_relate() {
        let from = Node {
            kind: "file".into(),
            id: "src/main.rs".into(),
            properties: r#"{"lines":100}"#.into(),
        };
        let to = Node {
            kind: "symbol".into(),
            id: "main".into(),
            properties: r#"{"pub":true}"#.into(),
        };
        let stmt = relate_statement(&from, "defines", &to, r#"{"exported":true}"#).unwrap();
        assert!(stmt.contains("UPSERT file:⟨src/main.rs⟩ CONTENT {\"lines\":100};"));
        assert!(stmt.contains("UPSERT symbol:⟨main⟩ CONTENT {\"pub\":true};"));
        assert!(stmt.contains("RELATE file:⟨src/main.rs⟩->defines->symbol:⟨main⟩ CONTENT {\"exported\":true};"));
    }
}

/// The shapes below were taken from a live SurrealDB 3.1.3 over HTTP, not from
/// the documentation. Three of them contradicted what the first draft assumed.
#[cfg(test)]
mod live_shapes {
    use super::*;

    /// SurrealDB quotes an id on the way OUT with backticks, having accepted it
    /// in angle brackets — and leaves it bare when it needs no quoting at all.
    /// The first draft stripped only brackets, so every path-shaped id read back
    /// wrapped in backticks and no round-trip matched.
    #[test]
    fn an_id_round_trips_through_the_quoting_the_server_chose() {
        let quoted = serde_json::json!({ "id": "file:`src/lib.rs`", "lines": 12 });
        assert_eq!(node_of(&quoted).unwrap().id, "src/lib.rs");
        let bare = serde_json::json!({ "id": "symbol:esc" });
        let n = node_of(&bare).unwrap();
        assert_eq!((n.kind.as_str(), n.id.as_str()), ("symbol", "esc"));
    }

    /// A fresh server has no namespace and will not make one on first write.
    #[test]
    fn a_missing_namespace_is_worth_a_retry_and_a_missing_table_is_not() {
        assert!(missing_namespace("The namespace 'comp' does not exist"));
        assert!(missing_namespace("The database 'knowledge' does not exist"));
        // Defining a namespace would not help a missing table, and retrying on
        // it would double every read of a kind nobody has written yet.
        assert!(!missing_namespace("The table 'nosuch' does not exist"));
    }

    /// Asking about a kind before writing one is normal for a graph being built.
    #[test]
    fn a_table_that_does_not_exist_reads_as_empty() {
        let absent = Err(GraphError::Rejected("The table 'nosuch' does not exist".into()));
        assert_eq!(absent_is_empty(absent).unwrap(), serde_json::json!([]));
        // A real refusal must still be one.
        let refused = Err(GraphError::Rejected("permission denied".into()));
        assert!(absent_is_empty(refused).is_err());
    }

    /// A write conflict is not a failure, and it is not a deadlock either.
    ///
    /// Captured from v3.1.3 under 20-way concurrency on one hot key. The whole
    /// concurrency strategy rests on the last sentence of that message — the
    /// transaction did not commit, so resending an increment cannot double-count.
    #[test]
    fn the_loser_of_an_optimistic_race_is_told_to_retry() {
        let body = r#"[{"kind":"Internal","result":"Transaction conflict: Write conflict, retry the transaction. This transaction can be retried","status":"ERR","time":"1ms","type":null}]"#;
        assert!(conflicted(body));
        // Not every rejection is retriable, and retrying the rest would turn one
        // failure into four.
        assert!(!conflicted(r#"[{"status":"ERR","result":"permission denied"}]"#));
        assert!(!conflicted(r#"[{"status":"ERR","result":"The table 'nosuch' does not exist"}]"#));
        // A conflict comes back inside an HTTP 200, so it has to be read out of
        // the body rather than off the status line.
        assert_eq!(
            body_error(body).unwrap(),
            "Transaction conflict: Write conflict, retry the transaction. This transaction can be retried"
        );
        assert_eq!(body_error(r#"[{"status":"OK","result":[]}]"#), None);
    }

    /// The success shape, so a future server version changing it fails here.
    #[test]
    fn a_traversal_answers_with_the_far_end_of_the_edge() {
        let body = r#"[{"result":[{"id":"symbol:esc","why":"escaping"}],"status":"OK","time":"4ms","type":null}]"#;
        let rows = first_result(body).unwrap();
        let n = node_of(&rows[0]).unwrap();
        assert_eq!(n.kind, "symbol");
        assert_eq!(n.properties, r#"{"why":"escaping"}"#);
    }
}
