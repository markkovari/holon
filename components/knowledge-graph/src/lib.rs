//! `knowledge-graph` — nodes, edges and traversal over SurrealDB's HTTP API.
//!
//! A component, not a host capability: it only needs a socket (`wasi:http/outgoing-handler`), so it is
//! deployable mid-run, swappable, and bounded by the egress allow-list (ADR-0008).
//!
//! Config via `wasi:config`: `surreal-url`, `surreal-ns`, `surreal-db`, `surreal-user`. The password is a
//! SECRET read through `comp:secrets/reader` as `surreal-password` (ADR-0010, ADR-0051).
//!
//! SurrealQL is built by string, so nothing a caller supplies is interpolated raw: ids go through
//! `record_id`, a kind or edge has to BE a table name, and properties are re-serialised from parsed
//! JSON. Tested against the injections they stop. The one exception is `query`, an escape hatch by design.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use bindings::comp::secrets::reader as secrets;
use bindings::exports::knowledge::graph::store::{Direction, GraphError, Guest, Node};
use bindings::wasi::config::store as config;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::{OutputStream, StreamError as OutputStreamError};

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
        // The password is the one thing that is not config. `none` is not fatal (a local database may have no
        // auth) but must send NO header: an empty-password `Basic root:` is refused by authenticated and
        // `--unauthenticated` servers alike.
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

/// A record id: `kind:⟨id⟩`. Angle brackets are SurrealDB's quoting for an arbitrary id (ULID, path,
/// name with a space). The KIND is restricted instead of quoted because it becomes a table name.
fn record_id(kind: &str, id: &str) -> Result<String, GraphError> {
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(GraphError::Rejected(format!("kind must be [a-zA-Z0-9_], got {kind:?}")));
    }
    Ok(format!("{kind}:⟨{}⟩", id.replace('⟩', "")))
}

/// An edge (table) name. Same rule as a kind, for the same reason.
fn edge_name(edge: &str) -> Result<String, GraphError> {
    if edge.is_empty() || !edge.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(GraphError::Rejected(format!("edge must be [a-zA-Z0-9_], got {edge:?}")));
    }
    Ok(edge.to_string())
}

/// A JSON object, or `{}`. Anything else is refused: `CONTENT` takes an object, and other shapes give
/// an error about SurrealQL rather than the caller's mistake.
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
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that rather than erroring, which
/// reaches the caller as an opaque `every replica failed`. `check-write` says how much the stream will take
/// now, so this writes in whatever bites it offers, blocks on the pollable when it offers nothing, and
/// flushes ONCE at the end (no flat 4096 chunking, one flush for a 40 KB statement).
fn write_all(stream: &OutputStream, mut bytes: &[u8]) -> Result<(), String> {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                // Zero is "full, wait", not an error: the pollable resolves when the stream has drained.
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
        let stream =
            body.write().map_err(|_| GraphError::Unavailable("no request stream".into()))?;
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
            Err(OutputStreamError::Closed) => break,
            // A failed read is not the end of the answer: parsing truncated bytes turns "half a result set" into
            // "nothing found" for a row that exists.
            Err(e) => return Err(GraphError::Unavailable(format!("reading the answer: {e:?}"))),
        }
    }
    let text = String::from_utf8_lossy(&out).to_string();
    if !(200..300).contains(&status) {
        // The database's own words, not a paraphrase.
        return Err(GraphError::Rejected(format!(
            "HTTP {status}: {}",
            text.chars().take(400).collect::<String>()
        )));
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

/// Send a statement, and if the namespace or database is not there yet, define it and send again. SurrealDB 3
/// does not create a namespace on first write, and requiring pre-provisioning would break fresh databases.
/// Retried ONCE: a second failure is the real answer.
fn missing_namespace(msg: &str) -> bool {
    msg.contains("does not exist") && (msg.contains("namespace") || msg.contains("database"))
}

/// How many times a conflicted statement is resent.
///
/// SurrealDB's transactions are OPTIMISTIC: the loser of a race is aborted and told to retry. Measured on
/// v3.1.3, 20-way on one hot key: 60 `SET uses += 1` give 53 commits and 7 rejections, and one resend clears
/// all 7. A conflicted transaction did NOT commit, so resending even a non-idempotent increment is safe.
/// No backoff: the winner committed before the loser heard, so the record is already free.
///
/// Twelve, not four: on a busy machine one resend does not always clear every conflict (59 or 58 of 60 while
/// compiling). A resend costs one round trip against a by-then uncontended database.
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

/// Send a statement, surviving the two things that are not really failures. Both live HERE rather than in
/// `run` because `query` (the escape hatch, used for `+=` and KNN by `knowledge-memory`) needs them too.
fn send(conn: &Conn, statement: &str) -> Result<String, GraphError> {
    let mut body = sql(conn, statement)?;

    // SurrealDB 3 does not create a namespace on first write: define it and retry ONCE.
    if let Some(msg) = body_error(&body).filter(|m| missing_namespace(m)) {
        sql(
            conn,
            &format!(
                "DEFINE NAMESPACE IF NOT EXISTS {}; DEFINE DATABASE IF NOT EXISTS {};",
                conn.ns, conn.db
            ),
        )
        // If defining is refused (scoped user without DEFINE rights), report the ORIGINAL statement's error, not the DEFINE's.
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
    // Out of attempts and STILL conflicted is a write that did not commit; it must not be returned as a
    // success (raw `query` callers would otherwise get an `Ok` whose contents say "retry the transaction").
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

/// A read of something that was never written is not an error. SurrealDB answers a read on a missing table
/// with "does not exist", but for a graph still being built the first question about a kind precedes its first write.
fn absent_is_empty(
    r: Result<serde_json::Value, GraphError>,
) -> Result<serde_json::Value, GraphError> {
    match r {
        Err(GraphError::Rejected(msg)) if msg.contains("does not exist") => {
            Ok(serde_json::Value::Array(vec![]))
        }
        other => other,
    }
}

/// A SurrealDB row back into a node. `id` comes back as `kind:the-id`, so the kind is stripped rather than
/// trusted from the caller's question (a traversal may return other kinds).
fn node_of(row: &serde_json::Value) -> Option<Node> {
    let full = row["id"].as_str()?;
    let (kind, id) = full.split_once(':')?;
    let mut props = row.clone();
    if let Some(o) = props.as_object_mut() {
        o.remove("id");
    }
    Some(Node {
        kind: kind.to_string(),
        // Ids go out in angle brackets and come back in backticks (only when quoting is needed); both are stripped so `upsert` then `get` round-trips.
        id: id.trim_matches(|c| c == '⟨' || c == '⟩' || c == '`').to_string(),
        properties: props.to_string(),
    })
}

/// Format a compound atomic relate statement: upserts both endpoint nodes and creates the edge in one
/// SurrealQL script, saving 3 HTTP round trips across the WASI boundary.
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
        let stmt =
            format!("UPSERT {} CONTENT {};", record_id(&n.kind, &n.id)?, object(&n.properties)?);
        run(&conn, &stmt).map(|_| ())
    }

    fn get(kind: String, id: String) -> Result<Option<Node>, GraphError> {
        let conn = Conn::open()?;
        let stmt = format!("SELECT * FROM {};", record_id(&kind, &id)?);
        let result = absent_is_empty(run(&conn, &stmt))?;
        Ok(result.as_array().and_then(|a| a.first()).and_then(node_of))
    }

    fn relate(
        from_node: Node,
        edge: String,
        to_node: Node,
        properties: String,
    ) -> Result<(), GraphError> {
        let conn = Conn::open()?;
        // Both ends and the edge go in ONE compound statement, so callers need not order writes and it stays idempotent.
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
        Ok(result.as_array().map(|a| a.iter().filter_map(node_of).collect()).unwrap_or_default())
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
        // Ids come from the world (paths, URLs, ULIDs), so they are quoted with SurrealDB's bracket form.
        assert_eq!(record_id("file", "a b/c.rs").unwrap(), "file:⟨a b/c.rs⟩");
        // A closing bracket is the one thing that could end quoting early, so it is removed rather than escaped.
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
        let to =
            Node { kind: "symbol".into(), id: "main".into(), properties: r#"{"pub":true}"#.into() };
        let stmt = relate_statement(&from, "defines", &to, r#"{"exported":true}"#).unwrap();
        assert!(stmt.contains("UPSERT file:⟨src/main.rs⟩ CONTENT {\"lines\":100};"));
        assert!(stmt.contains("UPSERT symbol:⟨main⟩ CONTENT {\"pub\":true};"));
        assert!(stmt.contains(
            "RELATE file:⟨src/main.rs⟩->defines->symbol:⟨main⟩ CONTENT {\"exported\":true};"
        ));
    }
}

/// The shapes below were taken from a live SurrealDB 3.1.3 over HTTP, not from the documentation.
#[cfg(test)]
mod live_shapes {
    use super::*;

    /// SurrealDB quotes an id on the way OUT with backticks (bare when no quoting is needed), having accepted
    /// angle brackets.
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
        // Defining a namespace would not help a missing table, and retrying would double every read of an unwritten kind.
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

    /// A write conflict is not a failure, and not a deadlock. Captured from v3.1.3: the transaction did not commit,
    /// so resending an increment cannot double-count.
    #[test]
    fn the_loser_of_an_optimistic_race_is_told_to_retry() {
        let body = r#"[{"kind":"Internal","result":"Transaction conflict: Write conflict, retry the transaction. This transaction can be retried","status":"ERR","time":"1ms","type":null}]"#;
        assert!(conflicted(body));
        // Not every rejection is retriable; retrying the rest would turn one failure into four.
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
