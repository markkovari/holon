//! `comp-field` — read one field out of a JSON document on stdin.
//!
//! The whole of what `components/gate-lib.sh` used `python3` for:
//!
//!     field() { python3 -c "import sys,json;print(json.load(sys.stdin).get('$1',''))"; }
//!
//! MEASURED, because "python is slow" deserves a number rather than a shrug. Forty
//! runs each, same document, same machine:
//!
//!     python3 -c json.load   15.3 ms
//!     jq -r .id               2.4 ms
//!     comp-field id           see `just gate-field-bench`
//!
//! 15 ms is interpreter start-up, paid on every field read of every response. There
//! are 106 `field` call sites across the gates, and the loop re-runs gates per
//! candidate per attempt — so it is multiplied by every branch of every graph, which
//! is where it stops being a rounding error.
//!
//! Not `jq`: it would be a second external dependency to install and check for, and
//! the gates already require `comp-plug` and `comp-host` at fixed paths. One more
//! binary from a workspace that is already built is not a new class of thing to have.
//!
//! ## What it does, exactly
//!
//! One top-level key, printed with a trailing newline, or an empty line when the key
//! is absent — matching `.get(key, '')` so that no gate has to change how it reads
//! the result. A document that is not an object, or not JSON at all, is also an empty
//! line: `python3 … 2>/dev/null` swallowed those, and a gate that starts failing
//! differently because its helper got stricter is a change nobody asked for.
//!
//! `--list` prints one line per element of an array-valued key, which is the other
//! shape the harness needed (`report_ids`). A DOTTED key walks nested objects —
//! `tokens.attendee.token` — which is what the gates spelled
//! `json.load(sys.stdin)['tokens']['attendee']['token']`.

fn main() {
    let mut args = std::env::args().skip(1);
    let mut list = false;
    let mut key = String::new();
    for a in args.by_ref() {
        match a.as_str() {
            "--list" => list = true,
            other => key = other.to_string(),
        }
    }
    if key.is_empty() {
        eprintln!("usage: comp-field [--list] <key>   # JSON on stdin");
        std::process::exit(2);
    }

    let mut body = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut body).is_err() {
        println!();
        return;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
        println!();
        return;
    };
    // A dotted path walks nested objects: `tokens.attendee.token`, which the gates
    // wrote as `json.load(sys.stdin)['tokens']['attendee']['token']`. A key with no
    // dot is a single lookup, so the common case is unchanged.
    let mut found = &value;
    for part in key.split('.') {
        match found.get(part) {
            Some(next) => found = next,
            None => {
                println!();
                return;
            }
        }
    }

    if list {
        if let Some(items) = found.as_array() {
            for item in items {
                println!("{}", scalar(item));
            }
        }
        return;
    }
    println!("{}", scalar(found));
}

/// A string prints as itself, anything else as its JSON.
///
/// Python's `print(value)` would render `True` and `None`, which no shell comparison
/// in this repository expects. A bare `to_string()` would quote every string, which
/// every comparison here would then fail against.
fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}
