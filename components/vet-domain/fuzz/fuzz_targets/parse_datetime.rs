#![no_main]

// The parser under test, included directly rather than linked as a dependency:
// `vet-domain` is a `cdylib`-only wasm component crate with a private `datetime`
// module, so this harness pulls the file in by path instead of reshaping the
// crate just to make it fuzzable.
#[path = "../../src/datetime.rs"]
mod datetime;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|s: &str| {
    // Only claim to look for panics/overflow, not a semantic oracle — `None`
    // for garbage is already covered by the unit tests in datetime.rs.
    let _ = datetime::parse_unix_seconds(s);
});
