//! W3C Trace Context: the ids every run, tool call and bus event carries, and
//! the `traceparent` header that carries them across HTTP.
//!
//! <https://www.w3.org/TR/trace-context/> — `00-<trace-id 32 hex>-<parent-id 16
//! hex>-<flags 2 hex>`, all lowercase, neither id all zeros. A caller that
//! already has a trace (a web request, another service) passes it in and this
//! runtime joins it; otherwise the first run starts one. Because the format is
//! the standard one, anything that speaks it — and every OpenTelemetry
//! collector — can follow a chain that crosses agents, the lattice and beyond.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(1);

/// 64 well-mixed random bits. `RandomState` is seeded per process from the OS,
/// and the counter and clock keep two calls from ever colliding.
fn rand64() -> u64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish()
}

fn nonzero64() -> u64 {
    loop {
        let v = rand64();
        if v != 0 {
            return v;
        }
    }
}

/// A new 32-hex trace id.
pub fn new_trace_id() -> String {
    format!("{:016x}{:016x}", nonzero64(), rand64())
}

/// A new 16-hex span id.
pub fn new_span_id() -> String {
    format!("{:016x}", nonzero64())
}

fn is_hex_id(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && s.bytes().any(|b| b != b'0')
}

#[derive(Debug, Clone, PartialEq)]
pub struct Traceparent {
    pub trace_id: String,
    /// The caller's span: this runtime's first span becomes its child.
    pub span_id: String,
    pub sampled: bool,
}

impl Traceparent {
    /// Parses a `traceparent` header value; `None` for anything malformed
    /// (the spec says to start a fresh trace rather than guess).
    pub fn parse(value: &str) -> Option<Self> {
        let mut parts = value.trim().split('-');
        let (version, trace, span, flags) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        // Version 00 has exactly four fields; later versions may append more.
        if version.len() != 2 || version == "ff" || (version == "00" && parts.next().is_some()) {
            return None;
        }
        if !is_hex_id(trace, 32) || !is_hex_id(span, 16) || flags.len() != 2 {
            return None;
        }
        let flags = u8::from_str_radix(flags, 16).ok()?;
        Some(Self {
            trace_id: trace.to_string(),
            span_id: span.to_string(),
            sampled: flags & 1 == 1,
        })
    }

    pub fn header(trace_id: &str, span_id: &str) -> String {
        format!("00-{trace_id}-{span_id}-01")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_have_the_standard_shape_and_are_unique() {
        let t = new_trace_id();
        let s = new_span_id();
        assert!(is_hex_id(&t, 32) && is_hex_id(&s, 16), "{t} {s}");
        let all: std::collections::HashSet<String> = (0..2000).map(|_| new_span_id()).collect();
        assert_eq!(all.len(), 2000);
    }

    #[test]
    fn parses_the_specs_own_example() {
        let tp =
            Traceparent::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01").unwrap();
        assert_eq!(tp.trace_id, "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(tp.span_id, "b7ad6b7169203331");
        assert!(tp.sampled);
        assert!(
            !Traceparent::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00")
                .unwrap()
                .sampled
        );
        assert_eq!(
            Traceparent::header(&tp.trace_id, &tp.span_id),
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        );
    }

    #[test]
    fn rejects_what_the_spec_says_to_reject() {
        for bad in [
            "",
            "garbage",
            "00-00000000000000000000000000000000-b7ad6b7169203331-01", // zero trace id
            "00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01", // zero span id
            "00-0AF7651916CD43DD8448EB211C80319C-b7ad6b7169203331-01", // uppercase
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331",    // no flags
            "ff-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01", // version ff
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-extra", // v00 is 4 fields
            "00-0af7651916cd43dd8448eb211c80319-b7ad6b7169203331-01",  // short trace id
        ] {
            assert!(Traceparent::parse(bad).is_none(), "{bad}");
        }
        // a future version may add fields
        assert!(Traceparent::parse("01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-more")
            .is_some());
    }
}
