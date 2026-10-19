//! The console's pieces that do not need a lattice to run: the agent board and the screenshot it
//! takes of itself. `main.rs` is the console proper (it boots a dev lattice); `bin/agents_board.rs`
//! is the same board over a scripted run, which is how it is developed and photographed.

pub mod board;
#[cfg(target_os = "macos")]
pub mod capture;
pub mod film;
