//! Scoping a goal to one component crate: deriving its build-scope from the
//! repository layout, and trimming a shared workspace manifest down to it.

/// The build-scope for a single component crate: the whole crate (src + wit +
/// Cargo.toml) plus the shared workspace root, whose members are trimmed to just
/// this crate. This is the "add the correct path" answer — name the component,
/// and the paths the gate needs come from the layout, not a hand-typed list.
pub(crate) fn component_scope(name: &str) -> (Vec<String>, String, Vec<String>) {
    (
        vec![format!("components/{name}/"), "components/Cargo.toml".to_string()],
        "components/Cargo.toml".to_string(),
        vec![name.to_string()],
    )
}

/// The index of the `]` that closes the array opened at `open`, ignoring any inside
/// a `#` comment.
///
/// TOML has no block comments and no escapes to worry about here: a `#` runs to the
/// end of the line, and a member is a quoted string that cannot contain a newline. A
/// `#` inside a quoted string is not a comment, so quotes are tracked too — a crate
/// named `"a#b"` is legal and would otherwise blind the rest of the line.
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

/// Rewrite a Cargo manifest's `members = [ … ]` to exactly `keep`.
///
/// A flat string edit rather than a toml round-trip, so the rest of the manifest
/// — `[workspace.package]`, `[workspace.dependencies]`, `[profile]`, every comment
/// — survives untouched; only the one array the gate needs narrowed is changed.
pub(crate) fn trim_members(manifest: &str, keep: &[String]) -> String {
    let Some(start) = manifest.find("members") else { return manifest.to_string() };
    let Some(open) = manifest[start..].find('[').map(|i| start + i) else {
        return manifest.to_string();
    };
    // The first `]` after the opening bracket is NOT necessarily the array's — a
    // comment inside the list can hold one, and `components/Cargo.toml`'s does:
    //
    //     members = [
    //         # `bench-suite-p3` stays out: it declares its own `[workspace]`, so
    //         "ai-inference",
    //
    // Closing on that `]` rewrote the manifest to `members = ["card-identify"]`,
    // so adding them is "multiple …` — invalid TOML. Every branch of every goal
    // scoped to this workspace then got a tree cargo refuses to load, scored zero,
    // and the gate had nothing to say about why. So: skip what a `#` comments out.
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

    /// The real `components/Cargo.toml`, whose members list opens with a comment
    /// containing `[workspace]` — so the first `]` after the bracket is not the
    /// array's. Closing on it produced invalid TOML, and every branch of every goal
    /// scoped to that workspace was handed a tree cargo refuses to load.
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
        // The whole point: what comes out has to be loadable.
        toml_is_parseable(&out);
    }

    /// Parsed with the same crate cargo would use, so "valid TOML" is not an opinion.
    fn toml_is_parseable(text: &str) {
        let parsed: Result<toml::Value, _> = toml::from_str(text);
        assert!(
            parsed.is_ok(),
            "the trimmed manifest is not valid TOML: {:?}\n{text}",
            parsed.err()
        );
    }
}
