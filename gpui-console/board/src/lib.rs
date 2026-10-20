//! The agent board: a live tree of the agents that are working, what each is doing with which model
//! and tools, what it has cost, and the ones that are waiting for a person (docs/adr/0119).
//!
//! Three layers, so the one that is hard to see (the tree) can be tested without a window and the
//! one that is easy to see (the pixels) can be changed without touching it:
//!
//! * [`tree`] is the model: nodes keyed by id, built by feeding it the runtime's [`Event`]s. Pure.
//! * [`map`] is the other two tabs' model: the components and the goal runs as frames of items, where
//!   they go, and the references between the three. Pure; [`store`] reads them.
//! * [`theme`] and [`icons`] are what it is drawn with.
//! * [`view`] is the GPUI view: the sessions, their trees, the cards and the motion; [`canvas`] draws a
//!   map on a sheet that pans and zooms.
//!
//! It builds natively and for a browser (wasm32), which is why it takes [`agent_wire`]'s data and not the
//! runtime's: the runtime does not build for a browser.
//!
//! [`Event`]: agent_wire::Event

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

/// The monospaced face: the system's natively, the one [`web_fonts`] brings in a browser, which has none.
#[cfg(not(target_family = "wasm"))]
pub const MONO: &str = "Menlo";
#[cfg(target_family = "wasm")]
pub const MONO: &str = "Lilex";

/// Where the repository is, which a proof's path is from: this crate's own place in it natively, and the
/// page's own origin in a browser (the board is served from the repository's root there too).
pub fn repo_root() -> std::path::PathBuf {
    #[cfg(not(target_family = "wasm"))]
    return std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    #[cfg(target_family = "wasm")]
    std::path::PathBuf::new()
}

/// The faces a browser draws the board in, since a page starts with none: IBM Plex Sans for the words,
/// Lilex for code (both SIL OFL 1.1, `fonts/README.md`).
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
