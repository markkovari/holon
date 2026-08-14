//! An exhaustive match on `Kind`, deliberately in a different file from the type.
use crate::kind::Kind;

pub fn render(k: &Kind) -> &'static str {
    match k {
        Kind::A => "a",
        Kind::B => "b",
    }
}
