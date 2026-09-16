#![no_main]

// `dates::days_from_civil` itself, included directly (no wit bindings needed —
// it's the pure Hinnant algorithm, moved out of lib.rs for exactly this).
#[path = "../../src/dates.rs"]
mod dates;

use libfuzzer_sys::fuzz_target;

// Mirrors `lib.rs::parse`'s split + bounds-check, since that function's real
// signature returns the wit-generated `RecurError` and isn't fuzzable without
// pulling in the whole component's bindings. Keep this in sync with `parse`.
fn parse(s: &str) -> Option<i64> {
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 3 {
        return None;
    }
    let y: i64 = p[0].parse().ok()?;
    let m: i64 = p[1].parse().ok()?;
    let d: i64 = p[2].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(-9999..=9999).contains(&y) {
        return None;
    }
    Some(dates::days_from_civil(y, m, d))
}

fuzz_target!(|s: &str| {
    let _ = parse(s);
});
