#![no_main]

// Included by path: `vet-domain` is a cdylib-only crate with a private `datetime` module.
#[path = "../../src/datetime.rs"]
mod datetime;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|s: &str| {
    // Panics and overflow only, not a semantic oracle.
    let _ = datetime::parse_unix_seconds(s);
});
