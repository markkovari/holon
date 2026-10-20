pub(crate) fn component_scope(name: &str) -> (Vec<String>, String, Vec<String>) {
    (
        vec![format!("components/{name}/"), "components/Cargo.toml".to_string()],
        "components/Cargo.toml".to_string(),
        vec![name.to_string()],
    )
}

/// Ignores a `]` inside a `#` comment; quotes are tracked since `"a#b"` is a legal crate name.
fn closing_bracket(text: &str, open: usize) -> Option<usize> {
    let mut in_comment = false;
    let mut in_string = false;
    for (i, c) in text.char_indices().skip_while(|(i, _)| *i <= open) {
        match c {
            '\n' => in_comment = false,
            '"' if !in_comment => in_string = !in_string,
            '#' if !in_string => in_comment = true,
            ']' if !in_comment && !in_string => return Some(i),
            _ => {}
        }
    }
    None
}

/// A string edit, not a toml round-trip, so profiles and comments survive untouched.
pub(crate) fn trim_members(manifest: &str, keep: &[String]) -> String {
    let Some(start) = manifest.find("members") else { return manifest.to_string() };
    let Some(open) = manifest[start..].find('[').map(|i| start + i) else {
        return manifest.to_string();
    };
    // A comment inside the list can hold a `]` (`components/Cargo.toml`'s does).
    let Some(close) = closing_bracket(manifest, open) else {
        return manifest.to_string();
    };
    let list = keep.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", ");
    format!("{}[{list}]{}", &manifest[..open], &manifest[close + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_component_name_derives_its_build_scope() {
        let (base_paths, manifest, members) = component_scope("rot13");
        assert_eq!(base_paths, ["components/rot13/", "components/Cargo.toml"]);
        assert_eq!(manifest, "components/Cargo.toml");
        assert_eq!(members, ["rot13"], "the workspace is trimmed to just this crate");
    }

    #[test]
    fn trimming_keeps_the_rest_of_the_manifest() {
        let m = "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\nresolver = \"2\"\n";
        let out = trim_members(m, &["b".to_string()]);
        assert!(out.contains("members = [\"b\"]"), "trimmed to the one member");
        assert!(out.contains("resolver = \"2\""), "the rest survives");
    }

    #[test]
    fn a_bracket_inside_a_comment_does_not_close_the_members_list() {
        let manifest = concat!(
            "[workspace]\n",
            "resolver = \"2\"\n",
            "members = [\n",
            "    # `bench-suite-p3` stays out: it declares its own `[workspace]`, so\n",
            "    # adding it is \"multiple workspace roots\".\n",
            "    \"ai-inference\",\n",
            "    \"card-identify\",\n",
            "]\n",
            "[workspace.package]\n",
            "version = \"0.1.0\"\n",
        );
        let out = trim_members(manifest, &["card-identify".to_string()]);
        assert!(out.contains("members = [\"card-identify\"]"), "{out}");
        assert!(!out.contains("ai-inference"), "the other members are gone: {out}");
        assert!(out.contains("[workspace.package]"), "the rest of the manifest survives: {out}");
        assert!(
            !out.contains("adding it is"),
            "the comment inside the list went with the list, leaving no dangling prose: {out}"
        );
        toml_is_parseable(&out);
    }

    fn toml_is_parseable(text: &str) {
        let parsed: Result<toml::Value, _> = toml::from_str(text);
        assert!(
            parsed.is_ok(),
            "the trimmed manifest is not valid TOML: {:?}\n{text}",
            parsed.err()
        );
    }
}
