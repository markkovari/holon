#![no_main]

// Mirrors `lib.rs::resolve`/`parse_field` exactly (can't `#[path]` include —
// they return the wit-generated `CronError`). Keep in sync.
use libfuzzer_sys::fuzz_target;

fn resolve(tok: &str, lo: u32, hi: u32) -> Option<u32> {
    let t = tok.trim().to_ascii_lowercase();
    let v: u32 = t.parse().ok()?;
    if v < lo || v > hi {
        return None;
    }
    Some(v)
}

fn parse_field(spec: &str, lo: u32, hi: u32) -> Option<(Vec<bool>, bool)> {
    let size = (hi - lo + 1) as usize;
    let mut bits = vec![false; size];
    let star = spec.trim() == "*";
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let (rng, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s.trim().parse().ok()?;
                (r, step)
            }
            None => (part, 1),
        };
        if step == 0 {
            return None;
        }
        let (start, end) = if rng == "*" {
            (lo, hi)
        } else if let Some((a, b)) = rng.split_once('-') {
            (resolve(a, lo, hi)?, resolve(b, lo, hi)?)
        } else {
            let v = resolve(rng, lo, hi)?;
            if part.contains('/') {
                (v, hi)
            } else {
                (v, v)
            }
        };
        if start > end {
            return None;
        }
        let mut v = start;
        while v <= end {
            bits[(v - lo) as usize] = true;
            v = match v.checked_add(step) {
                Some(next) => next,
                None => break,
            };
        }
    }
    Some((bits, star))
}

fuzz_target!(|s: &str| {
    // 0..=59, the widest of the five cron fields (minute/second-shaped).
    let _ = parse_field(s, 0, 59);
});
