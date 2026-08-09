//! Every fixture in the repo must parse.
//!
//! Fixtures are used by scripts that take a minute to reach the point where a bad one
//! would show up, and the failure there looks like "the benchmark is broken" rather
//! than "line 12 has a typo". This turns that into a sub-second test that names the
//! file — which is the whole reason the specs are typed rather than hand-built JSON.

use std::path::PathBuf;

use comp_reconciler::spec::AppSpec;

fn every_spec() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut out = Vec::new();
    let mut stack = vec![root.join("fixtures"), root.join("e2e"), root.join("bench")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "yaml") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_fixture_parses_and_resolves() {
    let specs = every_spec();
    assert!(specs.len() >= 10, "expected the repo's fixtures, found {}", specs.len());
    for path in specs {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        let spec = AppSpec::parse(&text).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        // Resolving is where a link naming a missing component, or a tenant nobody
        // supplied, would surface — so parse alone is not enough.
        let m = spec
            .to_manifest(Some("test-tenant"))
            .unwrap_or_else(|e| panic!("{name}: resolving: {e:#}"));

        assert!(!m.components.is_empty(), "{name}: no components");
        for c in &m.components {
            // The platform stamps this from the uploaded artifact. A fixture carrying
            // one would be pinning a digest nobody can reproduce.
            assert!(c.digest.is_empty(), "{name}: `{}` carries a digest", c.id);
        }
        // Every link must name components that exist, or the app deploys half-wired.
        let ids: Vec<&str> = m.components.iter().map(|c| c.id.as_str()).collect();
        for l in &m.links {
            assert!(ids.contains(&l.plug.as_str()), "{name}: link to unknown `{}`", l.plug);
            assert!(ids.contains(&l.socket.as_str()), "{name}: link from unknown `{}`", l.socket);
        }
    }
}

#[test]
fn the_caller_s_tenant_always_wins() {
    // Fixtures carry `tenant:` so they can run without a control plane. That must
    // never let a file override the org a real request is acting on.
    for path in every_spec() {
        let text = std::fs::read_to_string(&path).unwrap();
        let spec = AppSpec::parse(&text).unwrap();
        let m = spec.to_manifest(Some("caller")).unwrap();
        assert_eq!(m.tenant, "caller", "{}", path.display());
    }
}
