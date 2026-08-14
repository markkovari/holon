//! The type. A new variant here forces every exhaustive match on it — in other
//! files — to change too, or the crate does not compile.
pub enum Kind {
    A,
    B,
    C,
}