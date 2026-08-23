//! A `template` capability: substitute `{{key}}` placeholders from a set of
//! variables. Deliberately split across two files so the goal spans both — the
//! tokenizer finds the placeholders, the renderer resolves them.
pub mod render;
pub mod tokens;
pub use render::render;
