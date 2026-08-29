//! What a component IS, asked of the component rather than of a file about it.
//!
//! These rules used to live in `tools/gen-catalog.py`, which wrote their answers into
//! `components/catalog.json`, which `capsearch` then read back. So the loop opened a
//! 500 KB generated file to learn the result of a string check on a name it was
//! already holding — and the string check was wrong 33 times out of 212.
//!
//! The comment in `capsearch` even rationalised it — *"derived there rather than
//! re-guessed from the name here"* — but nothing was derived. It was the same name
//! check, performed somewhere else and written to disk, with all the staleness that
//! implies and none of the benefit.
//!
//! One of them is no longer a heuristic at all: whether a component can be plugged is
//! a fact about its exports, and it is now read off them.

use std::path::Path;

/// Can anything actually plug this component in?
///
/// This replaced a name check — `name.ends_with("-domain")` plus a hand-kept list of
/// ten exceptions — and the replacement is not tidiness. Measured across 212
/// components, the name disagreed with the component itself **33 times, in both
/// directions**:
///
///   * 30 were advertised as reusable while exporting nothing but
///     `wasi:http/incoming-handler`. Nothing can plug a door. Every probe was in
///     there, and so were `eshop-basket`, `-catalog`, `-gateway`, `-ordering` and
///     `-payment` — the five parts of one application, offered to a goal as though
///     each were a capability it could compose.
///   * 3 were hidden from search while exporting a real contract: `login-app`
///     exports `login:app/auth`, `reddit-domain` exports `local:reddit/reddit`, and
///     `power-domain` exports a bare `calculate-cost` function.
///
/// WHY THERE IS NO "APPLICATION" HERE ANY MORE. In the component model everything is
/// a component; "application" was this repository's own word, and it existed for one
/// job — stopping a showcase from outranking the capability it is built from. That
/// job does not need a category, it needs a property, and the property is this one. A
/// component exporting only WASI offers no contract for anyone to satisfy, so it
/// cannot be composed into anything, and saying so needs no convention, no suffix and
/// no list of exceptions to the convention.
///
/// A bare export name (`export generator;` inside a world) is a locally-defined
/// interface, so anything not in the `wasi:` namespace counts.
pub fn offers_a_contract<S: AsRef<str>>(exports: impl IntoIterator<Item = S>) -> bool {
    exports.into_iter().any(|e| !e.as_ref().starts_with("wasi:"))
}

/// The component's own one-line description: its first non-empty `//!`, trailing full
/// stop removed.
///
/// ADR-0094 is why this is the sentence that matters — it is the one a person wrote
/// next to the contract, in a caller's words. Read from the source rather than from a
/// generated copy of the source, so it cannot be stale.
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

    /// The cases the name check got wrong, in both directions.
    #[test]
    fn a_door_is_not_a_contract() {
        // What every "application" and every probe actually exports.
        assert!(!offers_a_contract(["wasi:http/incoming-handler@0.2.0"]));
        assert!(!offers_a_contract([] as [&str; 0]));
        // `eshop-basket` was called reusable by the name rule and exports only this.
        assert!(!offers_a_contract(["wasi:http/incoming-handler@0.2.0"]));

        // Hidden from search by the name rule, and plugable in fact.
        assert!(offers_a_contract(["login:app/auth@0.1.0"]));
        assert!(offers_a_contract(["local:reddit/reddit"]));
        // A bare name is a locally-defined interface: `world calc { export arith; }`.
        assert!(offers_a_contract(["arith"]));
        // A contract alongside a door is still a contract.
        assert!(offers_a_contract(["wasi:http/incoming-handler@0.2.0", "slug:generate/generator@0.1.0"]));
    }

    /// The description stops at the first line of real code, so a component with no
    /// module doc gets an empty string rather than the first comment it happens to
    /// contain.
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
