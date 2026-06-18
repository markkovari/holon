//! `jsonpatch` — reference implementation of `json:patch`.
//!
//! Pure-compute JSON document patching over three IETF standards:
//!   - RFC 6902 (JSON Patch): an ordered list of ops — add/remove/replace/
//!     move/copy/test — applied to a document. Application is atomic: every op
//!     runs against a working clone, and the result is returned only if all ops
//!     succeed, so the caller's original document is never partially mutated.
//!   - RFC 7386 (JSON Merge Patch): a sparse object overlay where a `null`
//!     value deletes a key and any other value is merged/replaced recursively.
//!   - RFC 6901 (JSON Pointer): the `/foo/0/~1bar` path syntax used to address
//!     locations within a document for RFC 6902 ops, including the `~1` -> `/`
//!     and `~0` -> `~` escapes and the array-append `-` token.
//!
//! `diff` produces an RFC 7386 merge-patch that transforms one document into
//! another. No state, no host imports.

#[allow(warnings)]
mod bindings;

use bindings::exports::json::patch::patcher::{Guest, PatchError};
use serde_json::Value;

struct Component;

// ---- JSON Pointer (RFC 6901) -------------------------------------------

/// Decode a single reference token: `~1` -> `/`, `~0` -> `~`.
fn unescape_token(tok: &str) -> String {
    tok.replace("~1", "/").replace("~0", "~")
}

/// Parse a JSON Pointer string into its decoded reference tokens. An empty
/// string addresses the whole-document root (zero tokens).
fn parse_pointer(path: &str) -> Result<Vec<String>, PatchError> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    if !path.starts_with('/') {
        return Err(PatchError::InvalidPatch(format!(
            "invalid JSON Pointer (must be empty or start with '/'): {path}"
        )));
    }
    // skip the leading '/' then split; "/" -> one empty token, etc.
    Ok(path[1..].split('/').map(unescape_token).collect())
}

/// Resolve an array index token (a non-negative integer with no leading zeros
/// beyond "0" itself).
fn parse_index(tok: &str, len: usize) -> Result<usize, PatchError> {
    if tok == "0" {
        return Ok(0);
    }
    if tok.is_empty() || tok.starts_with('0') || !tok.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PatchError::PathNotFound(format!(
            "invalid array index token: {tok}"
        )));
    }
    tok.parse::<usize>().map_err(|_| {
        PatchError::PathNotFound(format!("array index out of range: {tok} (len {len})"))
    })
}

/// Borrow the value at `tokens`, or `PathNotFound`.
fn resolve<'a>(root: &'a Value, tokens: &[String]) -> Result<&'a Value, PatchError> {
    let mut cur = root;
    for tok in tokens {
        cur = match cur {
            Value::Object(map) => map
                .get(tok)
                .ok_or_else(|| PatchError::PathNotFound(format!("no such key: {tok}")))?,
            Value::Array(arr) => {
                let idx = parse_index(tok, arr.len())?;
                arr.get(idx)
                    .ok_or_else(|| PatchError::PathNotFound(format!("index out of range: {idx}")))?
            }
            _ => {
                return Err(PatchError::PathNotFound(format!(
                    "cannot descend into scalar at token: {tok}"
                )))
            }
        };
    }
    Ok(cur)
}

/// Mutably borrow the *parent* container of the location named by `tokens`,
/// returning `(parent, last_token)`. `tokens` must be non-empty.
fn resolve_parent_mut<'a>(
    root: &'a mut Value,
    tokens: &'a [String],
) -> Result<(&'a mut Value, &'a String), PatchError> {
    let (last, parents) = tokens
        .split_last()
        .ok_or_else(|| PatchError::PathNotFound("empty path has no parent".into()))?;
    let mut cur = root;
    for tok in parents {
        cur = match cur {
            Value::Object(map) => map
                .get_mut(tok)
                .ok_or_else(|| PatchError::PathNotFound(format!("missing parent: {tok}")))?,
            Value::Array(arr) => {
                let len = arr.len();
                let idx = parse_index(tok, len)?;
                arr.get_mut(idx)
                    .ok_or_else(|| PatchError::PathNotFound(format!("missing parent: {idx}")))?
            }
            _ => {
                return Err(PatchError::PathNotFound(format!(
                    "parent is a scalar at token: {tok}"
                )))
            }
        };
    }
    Ok((cur, last))
}

// ---- RFC 6902 ops -------------------------------------------------------

/// Insert/replace `value` at `tokens`. For an array, an in-range index inserts
/// (shifting), "-" appends; for an object, the key is set. Root ("") replaces
/// the whole document.
fn op_add(root: &mut Value, tokens: &[String], value: Value) -> Result<(), PatchError> {
    if tokens.is_empty() {
        *root = value;
        return Ok(());
    }
    let (parent, last) = resolve_parent_mut(root, tokens)?;
    match parent {
        Value::Object(map) => {
            map.insert(last.clone(), value);
            Ok(())
        }
        Value::Array(arr) => {
            if last == "-" {
                arr.push(value);
                return Ok(());
            }
            let idx = parse_index(last, arr.len())?;
            if idx > arr.len() {
                return Err(PatchError::PathNotFound(format!(
                    "add index out of range: {idx} (len {})",
                    arr.len()
                )));
            }
            arr.insert(idx, value);
            Ok(())
        }
        _ => Err(PatchError::PathNotFound(
            "cannot add into a scalar parent".into(),
        )),
    }
}

/// Remove and return the value at `tokens`; missing -> PathNotFound.
fn op_remove(root: &mut Value, tokens: &[String]) -> Result<Value, PatchError> {
    if tokens.is_empty() {
        let old = std::mem::replace(root, Value::Null);
        return Ok(old);
    }
    let (parent, last) = resolve_parent_mut(root, tokens)?;
    match parent {
        Value::Object(map) => map
            .remove(last)
            .ok_or_else(|| PatchError::PathNotFound(format!("no such key to remove: {last}"))),
        Value::Array(arr) => {
            let idx = parse_index(last, arr.len())?;
            if idx >= arr.len() {
                return Err(PatchError::PathNotFound(format!(
                    "remove index out of range: {idx}"
                )));
            }
            Ok(arr.remove(idx))
        }
        _ => Err(PatchError::PathNotFound(
            "cannot remove from a scalar parent".into(),
        )),
    }
}

/// Replace an existing value at `tokens`; the location must already exist.
fn op_replace(root: &mut Value, tokens: &[String], value: Value) -> Result<(), PatchError> {
    // location must exist
    resolve(root, tokens)?;
    op_remove(root, tokens)?;
    op_add(root, tokens, value)
}

/// Read the required string field `field` from an op object.
fn req_str<'a>(op: &'a serde_json::Map<String, Value>, field: &str) -> Result<&'a str, PatchError> {
    op.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| PatchError::InvalidPatch(format!("op missing string field '{field}'")))
}

/// Read the required `value` field from an op object.
fn req_value<'a>(op: &'a serde_json::Map<String, Value>) -> Result<&'a Value, PatchError> {
    op.get("value")
        .ok_or_else(|| PatchError::InvalidPatch("op missing 'value' field".into()))
}

fn apply_op(root: &mut Value, op: &Value) -> Result<(), PatchError> {
    let obj = op
        .as_object()
        .ok_or_else(|| PatchError::InvalidPatch("patch op must be an object".into()))?;
    let name = req_str(obj, "op")?;
    match name {
        "add" => {
            let tokens = parse_pointer(req_str(obj, "path")?)?;
            op_add(root, &tokens, req_value(obj)?.clone())
        }
        "remove" => {
            let tokens = parse_pointer(req_str(obj, "path")?)?;
            op_remove(root, &tokens).map(|_| ())
        }
        "replace" => {
            let tokens = parse_pointer(req_str(obj, "path")?)?;
            op_replace(root, &tokens, req_value(obj)?.clone())
        }
        "move" => {
            let from = parse_pointer(req_str(obj, "from")?)?;
            let path = parse_pointer(req_str(obj, "path")?)?;
            let v = op_remove(root, &from)?;
            op_add(root, &path, v)
        }
        "copy" => {
            let from = parse_pointer(req_str(obj, "from")?)?;
            let path = parse_pointer(req_str(obj, "path")?)?;
            let v = resolve(root, &from)?.clone();
            op_add(root, &path, v)
        }
        "test" => {
            let tokens = parse_pointer(req_str(obj, "path")?)?;
            let want = req_value(obj)?;
            let got = resolve(root, &tokens)?;
            if got == want {
                Ok(())
            } else {
                Err(PatchError::TestFailed(format!(
                    "test failed at {}: expected {want}, found {got}",
                    req_str(obj, "path").unwrap_or("")
                )))
            }
        }
        other => Err(PatchError::InvalidPatch(format!("unknown op: {other}"))),
    }
}

// ---- RFC 7386 merge -----------------------------------------------------

/// Merge `patch` into `target` per RFC 7386 §2.
fn merge(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(patch_map) => {
            if !target.is_object() {
                *target = Value::Object(serde_json::Map::new());
            }
            let map = target.as_object_mut().expect("target made object above");
            for (k, v) in patch_map {
                if v.is_null() {
                    map.remove(k);
                } else {
                    let entry = map.entry(k.clone()).or_insert(Value::Null);
                    merge(entry, v);
                }
            }
        }
        other => {
            *target = other.clone();
        }
    }
}

// ---- diff (produces an RFC 7386 merge-patch) ----------------------------

/// Produce a merge-patch that turns `from` into `to`.
fn diff_value(from: &Value, to: &Value) -> Value {
    match (from, to) {
        (Value::Object(from_map), Value::Object(to_map)) => {
            let mut out = serde_json::Map::new();
            // keys present in `to`: add/update where missing or changed.
            for (k, to_v) in to_map {
                match from_map.get(k) {
                    Some(from_v) if from_v == to_v => {} // unchanged, omit
                    Some(from_v) => {
                        out.insert(k.clone(), diff_value(from_v, to_v));
                    }
                    None => {
                        out.insert(k.clone(), to_v.clone());
                    }
                }
            }
            // keys removed in `to`: delete via null.
            for k in from_map.keys() {
                if !to_map.contains_key(k) {
                    out.insert(k.clone(), Value::Null);
                }
            }
            Value::Object(out)
        }
        // not both objects (type change or scalar): the patch is `to` itself.
        _ => to.clone(),
    }
}

// ---- helpers ------------------------------------------------------------

fn parse_json(s: &str, what: &str) -> Result<Value, PatchError> {
    serde_json::from_str(s).map_err(|e| PatchError::InvalidJson(format!("{what}: {e}")))
}

fn to_string(v: &Value) -> Result<String, PatchError> {
    serde_json::to_string(v).map_err(|e| PatchError::InvalidJson(format!("serialize: {e}")))
}

impl Guest for Component {
    fn apply_patch(document: String, patch: String) -> Result<String, PatchError> {
        let mut working = parse_json(&document, "document")?;
        let patch_val = parse_json(&patch, "patch")?;
        let ops = patch_val
            .as_array()
            .ok_or_else(|| PatchError::InvalidPatch("patch must be a JSON array of ops".into()))?;
        // Atomic: every op runs on the working clone; we only serialize and
        // return it after all ops succeed, so on failure the caller's original
        // document is untouched.
        for op in ops {
            apply_op(&mut working, op)?;
        }
        to_string(&working)
    }

    fn apply_merge(document: String, merge_patch: String) -> Result<String, PatchError> {
        let mut target = parse_json(&document, "document")?;
        let patch = parse_json(&merge_patch, "merge-patch")?;
        merge(&mut target, &patch);
        to_string(&target)
    }

    fn diff(from: String, to: String) -> Result<String, PatchError> {
        let from_val = parse_json(&from, "from")?;
        let to_val = parse_json(&to, "to")?;
        to_string(&diff_value(&from_val, &to_val))
    }
}

bindings::export!(Component with_types_in bindings);
