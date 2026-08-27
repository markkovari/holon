//! `compose_to` writes into a content-addressed cache. Several callers may want the
//! same artifact at once — `tests/publish.rs` has three tests that each start a
//! control plane, and `cargo test` runs them as threads in one process.
//!
//! CI caught the consequence, once the harness started reporting why a host died:
//!
//!     Error: expected at least one module field
//!          --> components/target/composed/platform-domain.8bad9a2a41a087f0.wasm:1:1
//!
//! An EMPTY artifact. `fs::write` creates the file and then fills it, so a caller
//! checking `is_file()` in that window is handed a path to nothing.

use std::sync::{Arc, Barrier};

use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::{default_dirs, Catalog};

/// Every caller gets a whole component, not a file another one is still writing.
#[test]
fn composing_the_same_thing_from_several_threads_never_yields_a_partial_file() {
    let root = repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.bytes("platform-domain").is_none() {
        eprintln!("SKIPPED: platform-domain is not built — run `just build`");
        return;
    }

    // A fresh directory, so every thread races for the FIRST write rather than
    // finding the answer already there.
    let dir = tempfile::tempdir().unwrap();
    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));

    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let out = dir.path().to_path_buf();
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let catalog = Catalog::scan(&default_dirs(&root));
                barrier.wait();
                comp_reconciler::plug::compose_to("platform-domain", &catalog, &out)
            })
        })
        .collect();

    for h in handles {
        let path = h.join().expect("thread panicked").expect("compose failed");
        let bytes = std::fs::read(&path).expect("reading the composed artifact");
        assert!(
            bytes.len() > 1024,
            "{} is {} bytes — a caller was handed a file another was still writing",
            path.display(),
            bytes.len()
        );
        assert_eq!(&bytes[..4], b"\0asm", "{} does not start with the wasm magic", path.display());
        // Byte 6 distinguishes a component from a core module.
        assert_eq!(bytes[6], 0x01, "{} is not a component", path.display());
    }

    // And nothing is left behind: a temporary file in the cache directory would be
    // mistaken for an artifact by anything that globs it.
    let strays: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.ends_with(".wasm") || n.starts_with('.'))
        .collect();
    assert!(strays.is_empty(), "left behind: {strays:?}");
}
