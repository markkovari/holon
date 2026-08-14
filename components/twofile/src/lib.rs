//! A two-file goal: the spec (here) needs a variant added in `kind.rs` AND its
//! arm added in `render.rs`. Neither file alone compiles — that is the point.
pub mod kind;
pub mod render;

#[cfg(test)]
mod tests {
    use crate::kind::Kind;
    use crate::render::render;

    #[test]
    fn the_existing_variants_still_render() {
        assert_eq!(render(&Kind::A), "a");
        assert_eq!(render(&Kind::B), "b");
    }

    #[test]
    fn the_new_variant_renders() {
        assert_eq!(render(&Kind::C), "c");
    }
}
