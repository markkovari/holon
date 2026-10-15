//! What a component IS, asked of the component rather than of a file about it.
//!
//! Capability classification, read from the component itself. These rules once lived in
//! `tools/gen-catalog.py`, whose output `capsearch` read back from a 500 KB generated
//! `components/catalog.json`; the name check behind it was wrong 33 times out of 212.
//!
//! Whether a component can be plugged is a fact about its exports, and is read off them.

use std::path::Path;

/// Can anything actually plug this component in?
///
/// Across 212 components the old name check (`name.ends_with("-domain")` plus ten exceptions)
/// disagreed with the component **33 times, in both directions**:
///
/// * 30 were advertised as reusable while exporting nothing but `wasi:http/incoming-handler`
///   (nothing can plug a door), including the five parts of `eshop-*`;
/// * 3 were hidden from search while exporting a real contract (`login-app`, `reddit-domain`,
///   `power-domain`).
///
/// There is no "APPLICATION" category: it existed to stop a showcase outranking the capability it is
/// built from, which needs a property, not a category. A component exporting only WASI offers no
/// contract for anyone to satisfy, so it cannot be composed into anything.
///
/// A bare export name (`export generator;` inside a world) is a locally-defined interface, so
/// anything not in the `wasi:` namespace counts.
pub fn offers_a_contract<S: AsRef<str>>(exports: impl IntoIterator<Item = S>) -> bool {
    exports.into_iter().any(|e| !e.as_ref().starts_with("wasi:"))
}

/// The component's own one-line description: its first non-empty `//!`, trailing full stop removed.
///
/// ADR-0094: it is the sentence a person wrote next to the contract, in a caller's words. Read from
/// the source, not a generated copy, so it cannot be stale.
pub fn first_doc_line(lib_rs: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(lib_rs) else { return String::new() };
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("//!") {
            let t = rest.trim();
            if !t.is_empty() {
                return t.trim_end_matches('.').to_string();
            }
        } else if !line.is_empty() && !line.starts_with("//") {
            break;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_door_is_not_a_contract() {
        // What every "application" and every probe actually exports.
        assert!(!offers_a_contract(["wasi:http/incoming-handler@0.2.0"]));
        assert!(!offers_a_contract([] as [&str; 0]));
        // `eshop-basket` exports only this.
        assert!(!offers_a_contract(["wasi:http/incoming-handler@0.2.0"]));

        // Plugable in fact.
        assert!(offers_a_contract(["login:app/auth@0.1.0"]));
        assert!(offers_a_contract(["local:reddit/reddit"]));
        assert!(offers_a_contract(["arith"]));
        assert!(offers_a_contract([
            "wasi:http/incoming-handler@0.2.0",
            "slug:generate/generator@0.1.0"
        ]));
    }

    /// The description stops at the first line of real code, so a component with no module doc gets an
    /// empty string rather than the first comment it happens to contain.
    #[test]
    fn the_description_is_the_module_doc_and_nothing_else() {
        let dir = std::env::temp_dir().join("holon-catalogue-doc-test");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("lib.rs");

        std::fs::write(&f, "//! `slug` — turn a title into a clean URL slug.\n\nfn main() {}\n")
            .unwrap();
        assert_eq!(first_doc_line(&f), "`slug` — turn a title into a clean URL slug");

        std::fs::write(&f, "// an ordinary comment\nfn main() {}\n").unwrap();
        assert_eq!(first_doc_line(&f), "");

        std::fs::write(&f, "#[allow(warnings)]\nmod bindings;\n//! too late\n").unwrap();
        assert_eq!(first_doc_line(&f), "", "a doc line after real code is not the module doc");
    }
}
