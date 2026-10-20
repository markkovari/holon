//! The agent board (ADR-0119).

pub mod agents;
pub mod canvas;
#[cfg(target_os = "macos")]
pub mod capture;
pub mod demo;
pub mod explore;
pub mod field;
pub mod film;
pub mod graph;
pub mod icons;
pub mod map;
pub mod store;
pub mod theme;
pub mod tree;
pub mod view;

#[cfg(not(target_family = "wasm"))]
pub const MONO: &str = "Menlo";
#[cfg(target_family = "wasm")]
pub const MONO: &str = "Lilex";

pub fn repo_root() -> std::path::PathBuf {
    #[cfg(not(target_family = "wasm"))]
    return std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    #[cfg(target_family = "wasm")]
    std::path::PathBuf::new()
}

#[cfg(target_family = "wasm")]
pub fn web_fonts() -> Vec<std::borrow::Cow<'static, [u8]>> {
    use std::borrow::Cow;
    vec![
        Cow::Borrowed(include_bytes!("../fonts/IBMPlexSans-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../fonts/IBMPlexSans-SemiBold.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../fonts/Lilex-Regular.ttf").as_slice()),
    ]
}

pub use tree::{AgentTree, Kind, Node, Pending, Status, Summary};
pub use view::{Actions, AgentChoice, Board};
