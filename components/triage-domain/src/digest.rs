//! What the queue looks like, as a report. **This file is the goal of the `digest`
//! part.**
//!
//! `CONTRACT.md` is the specification. `crate::Reply` is how you answer.
//!
//! ## The capability you must not reimplement
//!
//! `crate::bindings::csv::codec::codec` formats CSV. One of the seeded reports is
//! titled `Login fails, silently` precisely so that `join(",")` produces a row with
//! the wrong number of columns — the gate parses the CSV with a real parser and counts.
//! It also reads the compiled component's imports, so formatting by hand fails even if
//! you get the quoting right.
//!
//! The surface, as wit-bindgen generates it in Rust:
//!
//!     use crate::bindings::csv::codec::codec as csv;
//!
//!     pub struct Row { pub fields: Vec<String> }
//!     pub struct Dialect {
//!         pub delimiter: String,   // a STRING, not a char: ",".to_string()
//!         pub has_header: bool,    // affects parse-records only
//!         pub trim: bool,
//!     }
//!     csv::format(rows: &[Row], opts: &Dialect) -> String
//!
//! A file is `Vec<Row>` — the header is just the first `Row` — and `format` returns the
//! whole document as one `String`.
//!
//! ## Answer the CSV with `Reply::raw`
//!
//! Not `Reply::json`. The router serialises a JSON body with `to_string()`, and a
//! `Value::String` serialises to a JSON string *literal* — surrounding quotes and `\"`
//! escapes included — so a correct CSV document would arrive as one quoted blob and a
//! CSV reader would see a single column:
//!
//!     Reply::raw(200, "text/csv", document.into_bytes())
//!
//! The gate checks the content type as well as the columns.
//!
//! ## One more thing about this workspace, because it cannot be guessed
//!
//! serde is built WITHOUT std here —
//!
//!     serde = { default-features = false, features = ["derive", "alloc"] }
//!
//! so `HashMap` has no `Serialize` impl and `json!({ "by_component": my_hashmap })`
//! fails with E0277. Use `BTreeMap<String, u32>`, which serde's alloc feature does
//! cover and which also gives a report the deterministic key order it wants.
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    // Replace this. Every route in CONTRACT.md's `digest` section, judged by real HTTP
    // requests against the running component.
    Reply::err(501, "not_implemented")
}
