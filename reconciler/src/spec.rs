//! The document a person writes, which is not the document the platform stores.
//!
//! The stored manifest (`plan::Manifest`) carries `digest: ""`, `host_needs`,
//! `egress` and `tenant` — all stamped by the platform: the digest by the upload, the
//! host needs by `wit-reflect`, the egress by policy (ADR-0008, never authored by a
//! tenant), the tenant by the org. Handing someone that document to fill in is the
//! actual reason the old JSON fixtures read badly; the syntax was never the problem.
//!
//! So there are two documents and a resolve step between them, which is the one idea
//! worth taking from both OAM (application vs configuration) and Kubernetes (spec vs
//! status).
//!
//! **Not OAM.** Its traits carry untyped `properties` bags, so every field check
//! happens at runtime — and the best errors this platform has are the typed ones
//! (ADR-0010's "unknown key, here are the legal ones"). Its links also name a
//! package, while ours name which import of which instance, so adopting it would mean
//! translating into our vocabulary anyway. It is the right IMPORT format, not the
//! right native one.
//!
//! **Not the Kubernetes envelope.** `apiVersion`/`kind`/`metadata`/`spec` promises
//! namespaces, selectors, update strategies and a status subresource — none of which
//! exist here, and ceremony without machinery is something a reader has to unlearn.
//! The one thing worth stealing is a version field, for the reason ADR-0044 gives.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::plan::{Component, Ingress, Link, Manifest, Placement, Scale, SecretRef, Strategy};

/// The only schema version. Present from the first file rather than added later,
/// because ADR-0044 is about exactly this and it is free now.
pub const VERSION: &str = "comp/v1";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
// A typo silently ignored is worse than a parse error: the author believes they
// configured something. Same rule as `comp.toml`.
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    /// `comp/v1`. Refused if it is anything else, rather than guessed at.
    pub version: String,
    pub app: String,
    /// Normally the org acting on the request. Explicit only where there is no
    /// control plane to supply it — fixtures, tests, `comp app plan` offline.
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub strategy: Strategy,
    pub components: Vec<ComponentSpec>,
    #[serde(default)]
    pub links: Vec<LinkSpec>,
    #[serde(default)]
    pub ingress: Option<IngressSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComponentSpec {
    pub id: String,
    /// Fixed replica count. Ignored when `scale` is present, which is why they are
    /// not both required.
    #[serde(default = "one")]
    pub replicas: u32,
    #[serde(default)]
    pub scale: Option<Scale>,
    #[serde(default)]
    pub placement: Placement,
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// By reference only. A value must never appear in a manifest (ADR-0010).
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    /// Normally stamped from the component's own WIT by `wit-reflect`, never
    /// authored — an author cannot know it and would get it wrong. Accepted here so a
    /// fixture can ask for a capability no host grants and assert the refusal.
    #[serde(default)]
    pub host_needs: Vec<String>,
    /// Stamped by platform policy, never by a tenant (ADR-0008). Same reasoning.
    #[serde(default)]
    pub egress: Vec<String>,
}

/// `from` imports `import`, and `to` provides it.
///
/// Deliberately plainer than the internal `plug`/`socket`, which read backwards to
/// everyone who has not internalised the composer's vocabulary. The conversion is
/// mechanical and happens once, here.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LinkSpec {
    /// The component whose import is being satisfied.
    pub from: String,
    /// The interface, exactly as WIT names it.
    pub import: String,
    /// The component that provides it.
    pub to: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IngressSpec {
    pub host: String,
    pub component: String,
}

fn one() -> u32 {
    1
}

impl AppSpec {
    pub fn parse(yaml: &str) -> Result<Self> {
        let spec: Self = serde_norway::from_str(yaml).context("reading the app spec")?;
        if spec.version != VERSION {
            bail!("unsupported spec version {:?}; this build understands {VERSION:?}", spec.version);
        }
        if spec.components.is_empty() {
            bail!("`{}` declares no components", spec.app);
        }
        // Catch a link naming a component that is not in the graph HERE, where the
        // file and the line are still in front of the author, rather than as an
        // unsatisfied import at deploy time.
        let ids: Vec<&str> = spec.components.iter().map(|c| c.id.as_str()).collect();
        for l in &spec.links {
            for (role, id) in [("from", &l.from), ("to", &l.to)] {
                if !ids.contains(&id.as_str()) {
                    bail!(
                        "link {role}: `{id}` is not a component of `{}` (have {ids:?})",
                        spec.app
                    );
                }
            }
        }
        if let Some(i) = &spec.ingress {
            if !ids.contains(&i.component.as_str()) {
                bail!("ingress names `{}`, which is not a component (have {ids:?})", i.component);
            }
        }
        Ok(spec)
    }

    /// Resolve into the document the platform stores.
    ///
    /// `tenant` comes from the caller — the org acting on the request — and only
    /// falls back to the file when there is none. `digest` is left empty on purpose:
    /// it is filled by the distribution pass from the uploaded artifact, and a digest
    /// an author could type would be a digest they could get wrong (ADR-0006).
    pub fn to_manifest(&self, tenant: Option<&str>) -> Result<Manifest> {
        let tenant = tenant
            .or(self.tenant.as_deref())
            .context("no tenant: pass one, or set `tenant:` in the spec")?
            .to_string();
        Ok(Manifest {
            app: self.app.clone(),
            tenant,
            strategy: self.strategy,
            components: self
                .components
                .iter()
                .map(|c| Component {
                    id: c.id.clone(),
                    digest: String::new(),
                    replicas: c.replicas,
                    scale: c.scale.clone(),
                    placement: c.placement.clone(),
                    host_needs: c.host_needs.clone(),
                    config: c.config.clone(),
                    secrets: c.secrets.clone(),
                    egress: c.egress.clone(),
                })
                .collect(),
            links: self
                .links
                .iter()
                .map(|l| Link {
                    // `to` provides, so it is the plug; `from` consumes, so it is the
                    // socket. This is the whole of the vocabulary change.
                    plug: l.to.clone(),
                    socket: l.from.clone(),
                    iface: l.import.clone(),
                })
                .collect(),
            ingress: self
                .ingress
                .as_ref()
                .map(|i| Ingress { host: i.host.clone(), component: i.component.clone() }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "
version: comp/v1
app: shop
tenant: acme
components:
  - id: gate
ingress:
  host: shop.acme.test
  component: gate
";

    #[test]
    fn the_minimum_a_person_has_to_write_is_short() {
        let spec = AppSpec::parse(MINIMAL).unwrap();
        let m = spec.to_manifest(None).unwrap();
        assert_eq!((m.app.as_str(), m.tenant.as_str()), ("shop", "acme"));
        assert_eq!(m.components.len(), 1);
        // Everything the platform stamps is absent from the file and empty here,
        // waiting to be filled by the parts of the system that know it.
        assert_eq!(m.components[0].digest, "");
        assert!(m.components[0].host_needs.is_empty());
        assert!(m.components[0].egress.is_empty());
        assert_eq!(m.components[0].replicas, 1, "a sensible default, not a required field");
    }

    #[test]
    fn from_import_to_becomes_socket_iface_plug() {
        // The one piece of vocabulary translation, and the easiest thing to get
        // backwards: `to` PROVIDES the interface, so it is the plug.
        let spec = AppSpec::parse(
            "
version: comp/v1
app: shop
tenant: acme
strategy: linked
components:
  - id: gate
  - id: store
links:
  - from: gate
    import: records:store/store@0.1.0
    to: store
",
        )
        .unwrap();
        let m = spec.to_manifest(None).unwrap();
        assert_eq!(m.links.len(), 1);
        assert_eq!(m.links[0].socket, "gate", "`from` consumes, so it is the socket");
        assert_eq!(m.links[0].plug, "store", "`to` provides, so it is the plug");
        assert_eq!(m.links[0].iface, "records:store/store@0.1.0");
    }

    #[test]
    fn the_caller_s_tenant_beats_the_file() {
        // In the real path the org comes from the request, and a `tenant:` in the
        // file must never let an author deploy into someone else's org.
        let spec = AppSpec::parse(MINIMAL).unwrap();
        assert_eq!(spec.to_manifest(Some("globex")).unwrap().tenant, "globex");
    }

    #[test]
    fn a_misspelled_key_is_refused_with_the_name_in_the_message() {
        // Formatted with `{:#}` — the whole chain — because that is what the CLI
        // prints and therefore what the author actually reads. `{}` would show only
        // "reading the app spec", which names no field and helps nobody.
        let err = format!(
            "{:#}",
            AppSpec::parse(
                "
version: comp/v1
app: shop
tenant: acme
components:
  - id: gate
    replicaz: 3
",
            )
            .unwrap_err()
        );
        assert!(err.contains("replicaz"), "{err}");
        assert!(err.contains("line"), "the message should point at a line: {err}");
    }

    #[test]
    fn a_link_to_a_component_that_is_not_there_is_caught_while_the_file_is_open() {
        let err = AppSpec::parse(
            "
version: comp/v1
app: shop
tenant: acme
strategy: linked
components:
  - id: gate
links:
  - from: gate
    import: records:store/store@0.1.0
    to: ghost
",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn an_unknown_version_is_refused_rather_than_guessed_at() {
        let err = AppSpec::parse("version: comp/v2\napp: shop\ncomponents:\n  - id: gate\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("comp/v2") && err.contains("comp/v1"), "{err}");
    }

    #[test]
    fn a_spec_round_trips_through_yaml() {
        // The converter has to be able to read what it writes, or `comp app show`
        // produces something that cannot be re-applied.
        let spec = AppSpec::parse(MINIMAL).unwrap();
        let out = serde_norway::to_string(&spec).unwrap();
        assert_eq!(AppSpec::parse(&out).unwrap(), spec, "{out}");
    }
}
