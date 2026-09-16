#![no_main]

#[path = "../../src/git.rs"]
mod git;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = git::parse_tree(data);
});
