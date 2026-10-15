//! `config-store` — runtime application config — timeouts, limits, URLs and tunables ops can change without a redeploy
//!
//! The *writable, versioned* sibling of `wasi:config` (host-injected, read-only, fixed at
//! deploy time): typed values (string / int / bool / float / json) the *app itself* manages
//! at runtime, every write bumping a monotonic version for optimistic concurrency and cache
//! invalidation. `feature-flags` is for *behaviour decisions*; this is for *raw values*
//! (timeouts, limits, URLs, tunables).
//! Backed purely by `wasi:keyvalue` + `wasi:clocks`. State per key lives at
//! `cfg_{namespace}_{key}` as one serialized line:
//!   `{tag}\t{version}\t{updated}\t{payload}`
//! where `tag` ∈ {s,i,b,f,j} selects the `value` variant, `payload` renders
//! the value as text (ints/floats via `to_string`, bools as `true`/`false`,
//! text/json base64-encoded so tab/newline delimiters survive). A per-namespace
//! key index lives at `cfgidx_{namespace}` as newline-joined key names.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use bindings::exports::config::store::store::{ConfigError, Entry, Guest, Value};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";

fn now() -> u64 {
    wall_clock::now().seconds
}

/// Sanitize one opaque segment to NATS-legal kv chars (same byte scheme as
/// idempotency-guard's `id_key` / the rate-limiter's `rl_key`).
fn sanitize(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

/// Storage key for a config entry: `cfg_{namespace}_{key}`, each segment
/// sanitized independently so the join `_` is never ambiguous.
fn cfg_key(namespace: &str, key: &str) -> String {
    format!("cfg_{}_{}", sanitize(namespace), sanitize(key))
}

/// Storage key for a namespace's key-name index.
fn idx_key(namespace: &str) -> String {
    format!("cfgidx_{}", sanitize(namespace))
}

/// Render a `value` to its `{tag}\t{payload}` parts.
fn encode_value(value: &Value) -> (char, String) {
    match value {
        Value::Text(s) => ('s', B64.encode(s.as_bytes())),
        Value::Integer(i) => ('i', i.to_string()),
        Value::Boolean(b) => ('b', if *b { "true".into() } else { "false".into() }),
        Value::Decimal(f) => ('f', f.to_string()),
        Value::Json(s) => ('j', B64.encode(s.as_bytes())),
    }
}

fn encode_entry(value: &Value, version: u32, updated: u64) -> String {
    let (tag, payload) = encode_value(value);
    format!("{tag}\t{version}\t{updated}\t{payload}")
}

fn decode_b64_str(payload: &str, what: &str) -> Result<String, ConfigError> {
    let bytes = B64
        .decode(payload)
        .map_err(|_| ConfigError::TypeMismatch(format!("corrupt {what} payload (base64)")))?;
    String::from_utf8(bytes)
        .map_err(|_| ConfigError::TypeMismatch(format!("corrupt {what} payload (utf-8)")))
}

/// Parse a stored line back into an `entry`. Any structural or value parse
/// failure surfaces as `type-mismatch` with context.
fn parse_entry(s: &str) -> Result<Entry, ConfigError> {
    let mut parts = s.splitn(4, '\t');
    let tag = parts.next().ok_or_else(|| ConfigError::TypeMismatch("missing type tag".into()))?;
    let version = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| ConfigError::TypeMismatch("corrupt entry: version".into()))?;
    let updated = parts
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| ConfigError::TypeMismatch("corrupt entry: updated".into()))?;
    let payload = parts.next().unwrap_or("");

    let value = match tag {
        "s" => Value::Text(decode_b64_str(payload, "text")?),
        "i" => Value::Integer(
            payload
                .parse::<i64>()
                .map_err(|_| ConfigError::TypeMismatch("corrupt entry: integer".into()))?,
        ),
        "b" => match payload {
            "true" => Value::Boolean(true),
            "false" => Value::Boolean(false),
            _ => return Err(ConfigError::TypeMismatch("corrupt entry: boolean".into())),
        },
        "f" => Value::Decimal(
            payload
                .parse::<f64>()
                .map_err(|_| ConfigError::TypeMismatch("corrupt entry: decimal".into()))?,
        ),
        "j" => Value::Json(decode_b64_str(payload, "json")?),
        other => return Err(ConfigError::TypeMismatch(format!("unknown type tag {other:?}"))),
    };

    Ok(Entry { value, version, updated })
}

fn open() -> Result<kv::Bucket, ConfigError> {
    kv::open(BUCKET).map_err(|e| ConfigError::BackendUnavailable(format!("open: {e:?}")))
}

fn load_entry(bucket: &kv::Bucket, k: &str) -> Result<Option<Entry>, ConfigError> {
    match bucket.get(k) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| ConfigError::TypeMismatch("stored value not utf-8".into()))?;
            Ok(Some(parse_entry(&s)?))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(ConfigError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn put_raw(bucket: &kv::Bucket, k: &str, body: &str) -> Result<(), ConfigError> {
    bucket
        .set(k, body.as_bytes())
        .map_err(|e| ConfigError::BackendUnavailable(format!("set: {e:?}")))
}

// The index is a newline-joined list of sanitized key names, maintained best-effort by
// read-modify-write on a key's first `set` and on `delete`. Single-writer: concurrent
// add/add or add/delete can drop or duplicate an entry, since wasi:keyvalue@0.2.0-draft has
// no compare-and-swap. The entries stay authoritative; the index is a listing convenience.

fn read_index(bucket: &kv::Bucket, namespace: &str) -> Result<Vec<String>, ConfigError> {
    match bucket.get(&idx_key(namespace)) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| ConfigError::BackendUnavailable("index not utf-8".into()))?;
            Ok(s.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect())
        }
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(ConfigError::BackendUnavailable(format!("get index: {e:?}"))),
    }
}

fn write_index(bucket: &kv::Bucket, namespace: &str, names: &[String]) -> Result<(), ConfigError> {
    bucket
        .set(&idx_key(namespace), names.join("\n").as_bytes())
        .map_err(|e| ConfigError::BackendUnavailable(format!("set index: {e:?}")))
}

fn index_add(bucket: &kv::Bucket, namespace: &str, key: &str) -> Result<(), ConfigError> {
    let mut names = read_index(bucket, namespace)?;
    if !names.iter().any(|n| n == key) {
        names.push(key.to_string());
        write_index(bucket, namespace, &names)?;
    }
    Ok(())
}

fn index_remove(bucket: &kv::Bucket, namespace: &str, key: &str) -> Result<(), ConfigError> {
    let mut names = read_index(bucket, namespace)?;
    let before = names.len();
    names.retain(|n| n != key);
    if names.len() != before {
        write_index(bucket, namespace, &names)?;
    }
    Ok(())
}

impl Guest for Component {
    fn get(namespace: String, key: String) -> Result<Entry, ConfigError> {
        let bucket = open()?;
        let k = cfg_key(&namespace, &key);
        load_entry(&bucket, &k)?.ok_or(ConfigError::NotFound)
    }

    fn set(namespace: String, key: String, value: Value) -> Result<u32, ConfigError> {
        let bucket = open()?;
        let k = cfg_key(&namespace, &key);
        let existed = load_entry(&bucket, &k)?;
        let current = existed.as_ref().map(|e| e.version).unwrap_or(0);
        let new = current + 1;
        put_raw(&bucket, &k, &encode_entry(&value, new, now()))?;
        if existed.is_none() {
            index_add(&bucket, &namespace, &key)?;
        }
        Ok(new)
    }

    fn set_if(
        namespace: String,
        key: String,
        value: Value,
        expected_version: u32,
    ) -> Result<u32, ConfigError> {
        let bucket = open()?;
        let k = cfg_key(&namespace, &key);
        let existed = load_entry(&bucket, &k)?;
        let current = existed.as_ref().map(|e| e.version).unwrap_or(0);
        if current != expected_version {
            return Err(ConfigError::VersionConflict(current));
        }
        let new = current + 1;
        put_raw(&bucket, &k, &encode_entry(&value, new, now()))?;
        if existed.is_none() {
            index_add(&bucket, &namespace, &key)?;
        }
        Ok(new)
    }

    fn delete(namespace: String, key: String) -> Result<bool, ConfigError> {
        let bucket = open()?;
        let k = cfg_key(&namespace, &key);
        let existed = load_entry(&bucket, &k)?.is_some();
        if existed {
            bucket
                .delete(&k)
                .map_err(|e| ConfigError::BackendUnavailable(format!("delete: {e:?}")))?;
            index_remove(&bucket, &namespace, &key)?;
        }
        Ok(existed)
    }

    fn keys(namespace: String, max: u32) -> Result<Vec<String>, ConfigError> {
        let bucket = open()?;
        let mut names = read_index(&bucket, &namespace)?;
        names.truncate(max as usize);
        Ok(names)
    }
}

bindings::export!(Component with_types_in bindings);
