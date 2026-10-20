use std::path::Path;

/// Read from exports: the old `-domain` name check was wrong 33 of 212 times. A component
/// exporting only `wasi:` offers no contract; a bare export name is local, so it counts.
pub fn offers_a_contract<S: AsRef<str>>(exports: impl IntoIterator<Item = S>) -> bool {
    exports.into_iter().any(|e| !e.as_ref().starts_with("wasi:"))
}

/// Read from the source, not a generated copy, so it cannot be stale (ADR-0094).
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
        assert!(!offers_a_contract(["wasi:http/incoming-handler@0.2.0"]));
        assert!(!offers_a_contract([] as [&str; 0]));
        assert!(!offers_a_contract(["wasi:http/incoming-handler@0.2.0"]));

        assert!(offers_a_contract(["login:app/auth@0.1.0"]));
        assert!(offers_a_contract(["local:reddit/reddit"]));
        assert!(offers_a_contract(["arith"]));
        assert!(offers_a_contract([
            "wasi:http/incoming-handler@0.2.0",
            "slug:generate/generator@0.1.0"
        ]));
    }

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
