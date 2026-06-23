//! `cache-backing` — exports cache:store's `source` + `sink` over wasi:keyvalue.
//!
//! cache:store needs an authoritative backing store behind its TTL cache. This
//! is it: load/store/remove against a dedicated keyvalue bucket. Composing it
//! into an app that uses cache:store leaves zero non-WASI imports, so the app
//! deploys on wasmCloud (keyvalue -> keyvalue-nats) with no special host code.

#[allow(warnings)]
mod bindings;

use bindings::exports::cache::store::sink::Guest as SinkGuest;
use bindings::exports::cache::store::source::Guest as SourceGuest;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
const PREFIX: &str = "cb_"; // keep cache-backing keys out of the app's namespace

fn open() -> Result<kv::Bucket, String> {
    kv::open(BUCKET).map_err(|e| format!("open: {e:?}"))
}

impl SourceGuest for Component {
    fn load(key: String) -> Result<Option<Vec<u8>>, String> {
        let b = open()?;
        b.get(&format!("{PREFIX}{key}")).map_err(|e| format!("get: {e:?}"))
    }
}

impl SinkGuest for Component {
    fn store(key: String, value: Vec<u8>) -> Result<(), String> {
        let b = open()?;
        b.set(&format!("{PREFIX}{key}"), &value).map_err(|e| format!("set: {e:?}"))
    }
    fn remove(key: String) -> Result<(), String> {
        let b = open()?;
        b.delete(&format!("{PREFIX}{key}")).map_err(|e| format!("delete: {e:?}"))
    }
}

bindings::export!(Component with_types_in bindings);
