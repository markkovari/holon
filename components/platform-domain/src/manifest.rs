//! The manifest: `(graph, strategy, tenant, plan) -> desired state`.
//!
//! Successor to `render.rs`, which turned the same inputs into Kubernetes YAML by
//! string concatenation. There is no Kubernetes any more (ADR-0021), so this emits
//! the document the reconciler diffs against lattice inventory instead.
//!
//! Still a pure function, and for the same reason: it decides which store a
//! workload may touch and where it may dial, so it has no I/O, no clock, and
//! nothing it could get wrong asynchronously. Everything arrives as an argument.
//!
//! The vocabulary shrank a lot, which is the point. A manifest names components by
//! digest, the links between them, where they may run, and what they may reach.
//! Namespaces, quotas, network policies, pod specs, sidecars and volume claims are
//! all gone — they were Kubernetes' way of saying things the host now enforces in
//! its own linker.
//!
//! Two rules survived the move because they encode expensive lessons:
//!
//! 1. **A digest, never a tag** (ADR-0006). Re-deriving desired state must select
//!    the same bytes it selected last time.
//! 2. **`egress` needs both the bare and port-qualified forms.** Observed in
//!    `examples/jobs/k8s/jobs.yaml`; egress is fail-closed, so a missing form is a
//!    silent connection refusal at runtime rather than an error at deploy.

use std::collections::BTreeSet;

use serde_json::{json, Value};

/// What a tenant's plan permits. Stamped by the platform, never tenant-authored
/// (ADR-0008).
#[derive(Clone, Debug)]
pub struct Plan {
    pub replicas: u32,
    /// Warm pre-instantiated instances, capped against the node's core-instance
    /// budget — see `safe_pool_size`.
    pub pool_size: u32,
    pub max_invocations: u32,
    /// Destinations this tenant may dial, emitted both bare and port-qualified.
    /// Empty means egress is denied entirely.
    pub egress: Vec<String>,
    /// Node labels a deployment must match. The multicloud/multiregion knob.
    pub constraints: Vec<(String, String)>,
}

impl Default for Plan {
    fn default() -> Self {
        // Mirrors what the vet-clinic settled on after `poolSize: 48` starved a
        // host (1344 core instances against a 1000 cap) — ADR-0008.
        Plan {
            replicas: 1,
            pool_size: 8,
            max_invocations: 200,
            egress: Vec::new(),
            constraints: Vec::new(),
        }
    }
}

/// How the graph becomes something running (ADR-0005).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// One artifact, composed by `wit:reflect` before deploy.
    Fused,
    /// N components the host links in-process from a link table.
    Linked,
}

impl Strategy {
    pub fn parse(s: &str) -> Option<Strategy> {
        match s {
            "fused" => Some(Strategy::Fused),
            "linked" => Some(Strategy::Linked),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Strategy::Fused => "fused",
            Strategy::Linked => "linked",
        }
    }
}

/// One component as the manifest needs it.
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    /// Config this component was given, already validated against the keys its
    /// uploader declared. Empty is legal; unknown is not, and is refused before a
    /// `Part` is ever built (ADR-0010).
    pub config: std::collections::BTreeMap<String, String>,
    /// Secrets this component asks for, BY REFERENCE. Validated at save — every ref
    /// resolves and belongs to this org — and the value is never read, so it cannot
    /// reach a manifest, a revision, or a log (ADR-0010).
    pub secrets: Vec<(String, String)>,
    /// The content address of the bytes — `sha256:...`, bare. ADR-0024: the digest
    /// IS the identity, and a node fetches by it from the object store.
    pub digest: String,
    /// Host imports as `ns:pkg/iface` triples, from `wit:reflect`. Stamped into the
    /// manifest so the reconciler can place with a set comparison and never needs
    /// to inspect a component itself.
    pub host_imports: Vec<HostIface>,
    /// Nested core modules, for the pool-size ceiling.
    pub nested_instances: u32,
    /// Does this export `wasi:http/incoming-handler`?
    pub serves_http: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostIface {
    pub namespace: String,
    pub pkg: String,
    pub iface: String,
}

impl HostIface {
    /// The form a node advertises in its inventory, and the form the reconciler
    /// matches against. Versionless on purpose: a node advertises a concrete
    /// version and a component imports one, and requiring the strings to be equal
    /// would make a patch bump unschedulable.
    // ponytail: family match; tighten to semver when two incompatible versions of
    // one interface actually coexist.
    fn family(&self) -> String {
        format!("{}:{}/{}", self.namespace, self.pkg, self.iface)
    }
}

/// Host interface families a node can be asked for.
///
/// The successor to `OPERATOR_BOUND`, and still the highest-consequence list here:
/// everything on it is something a tenant's component may import. Everything a
/// component imports from `wasi:*` that is NOT on it — `cli`, `io`, `clocks`,
/// `random`, `filesystem` — is ambient, provided without being asked, and must not
/// appear in `host_needs` or every deployment would be unschedulable.
///
/// `wasi:http/types` is the trap: components import it (the request/response types
/// live there) but it is type-only and has no backend, so it is not listed.
const SCHEDULABLE: &[(&str, &str, &[&str])] = &[
    ("wasi", "http", &["incoming-handler", "outgoing-handler"]),
    ("wasi", "keyvalue", &["store", "atomics", "batch"]),
    ("wasi", "config", &["store"]),
    ("wasi", "blobstore", &["blobstore", "container"]),
];

fn schedulable(h: &HostIface) -> bool {
    SCHEDULABLE.iter().any(|(ns, pkg, ifaces)| {
        *ns == h.namespace && *pkg == h.pkg && ifaces.contains(&h.iface.as_str())
    })
}

pub struct ManifestInput<'a> {
    pub tenant: &'a str,
    /// Deployment name, validated as a DNS label by `build`.
    pub name: &'a str,
    pub strategy: Strategy,
    pub parts: &'a [Part],
    pub plan: &'a Plan,
    /// `(plug, socket, iface)` — `composer.edge`, the same vocabulary the canvas
    /// emits and the reconciler turns into a link table.
    pub edges: &'a [(String, String, String)],
    /// The component traffic enters through.
    pub root: &'a str,
    /// The Host header this app answers to.
    pub ingress_host: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// A component with no content address. ADR-0006 makes this fatal: re-deriving
    /// desired state must select the same bytes it selected last time.
    NotDigestPinned(String),
    Empty,
    BadName(String),
    /// `fused` must arrive as exactly one part — the composed artifact.
    FusedNotComposed(usize),
    /// The named root is not in the graph.
    NoRoot(String),
}

impl ManifestError {
    pub fn detail(&self) -> String {
        match self {
            ManifestError::NotDigestPinned(c) => format!(
                "component `{c}` has no digest yet — it has not been distributed. A tag cannot be re-derived reproducibly (ADR-0006)."
            ),
            ManifestError::Empty => "a deployment needs at least one component".into(),
            ManifestError::BadName(n) => format!("`{n}` is not a valid DNS label"),
            ManifestError::FusedNotComposed(n) => format!(
                "the fused strategy deploys one composed artifact, got {n} parts — compose first"
            ),
            ManifestError::NoRoot(r) => format!("`{r}` is not a component in this graph"),
        }
    }
}

/// One application's identity across the fleet. Derived from tenant + name, never
/// supplied: a tenant able to set this would be a tenant able to name someone
/// else's storage. The host derives its bucket from the same rule.
pub fn env_for(tenant: &str, name: &str) -> String {
    let e = format!("app-{}-{}", dns_label(tenant), dns_label(name));
    e.chars().take(53).collect::<String>().trim_matches('-').to_string()
}

pub fn dns_label(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    out.trim_matches('-').to_string()
}

pub fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s == dns_label(s)
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
}

/// `poolSize × modules-per-instance` has to stay under a node's concurrent
/// core-instance cap. The vet-clinic proved the failure: pool 48 over a ~28-module
/// graph starved a 1000-instance host. Clamp rather than trust the plan.
pub fn safe_pool_size(requested: u32, total_modules: u32) -> u32 {
    const CORE_INSTANCE_BUDGET: u32 = 800; // of ~1000, leaving headroom
    let modules = total_modules.max(1);
    let ceiling = (CORE_INSTANCE_BUDGET / modules).max(1);
    requested.clamp(1, ceiling)
}

/// Expand an egress allow-list into every form an authority can arrive in.
///
/// * A scheme-qualified entry passes through untouched. Splitting it on `:` to find
///   a port would turn `https://api.example.com` into the host `https` — silently
///   allow-listing the wrong thing, since `https` is itself a legal host.
/// * A bare authority is emitted both bare and port-qualified, because egress is
///   fail-closed and a missing form is a connection refused at runtime rather than
///   an error at deploy.
pub fn expand_egress(egress: &[String]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for e in egress {
        let e = e.trim();
        if e.is_empty() {
            continue;
        }
        if e == "*" || e.contains("://") {
            out.insert(e.to_string());
            continue;
        }
        match e.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
                out.insert(host.to_string());
                out.insert(e.to_string());
            }
            _ => {
                out.insert(e.to_string());
                out.insert(format!("{e}:80"));
                out.insert(format!("{e}:443"));
            }
        }
    }
    out.into_iter().collect()
}

/// Build the desired-state document.
pub fn build(input: &ManifestInput) -> Result<Value, ManifestError> {
    if input.parts.is_empty() {
        return Err(ManifestError::Empty);
    }
    if !is_dns_label(input.name) {
        return Err(ManifestError::BadName(input.name.to_string()));
    }
    if input.strategy == Strategy::Fused && input.parts.len() != 1 {
        return Err(ManifestError::FusedNotComposed(input.parts.len()));
    }
    for p in input.parts {
        if !p.digest.starts_with("sha256:") {
            return Err(ManifestError::NotDigestPinned(p.name.clone()));
        }
        if !is_dns_label(&p.name) {
            return Err(ManifestError::BadName(p.name.clone()));
        }
    }
    if !input.parts.iter().any(|p| p.name == input.root) {
        return Err(ManifestError::NoRoot(input.root.to_string()));
    }

    let total_modules: u32 = input.parts.iter().map(|p| p.nested_instances.max(1)).sum();
    let pool = safe_pool_size(input.plan.pool_size, total_modules);
    let egress = expand_egress(&input.plan.egress);
    let constraints: serde_json::Map<String, Value> = input
        .plan
        .constraints
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();

    let components: Vec<Value> = input
        .parts
        .iter()
        .map(|p| {
            let is_root = p.name == input.root;
            // Only the entry point is replicated. A plug is linked in-process by
            // whatever calls it, so a second copy on the same node would be a second
            // idle `InstancePre` and nothing else.
            let replicas = if is_root { input.plan.replicas } else { 1 };
            let host_needs: Vec<String> = p
                .host_imports
                .iter()
                .filter(|h| schedulable(h))
                .map(|h| h.family())
                .collect();
            json!({
                "id": p.name,
                "digest": p.digest,
                "replicas": replicas,
                "placement": {
                    "mode": "spread",
                    "nodes": [],
                    "constraints": constraints,
                },
                "host_needs": host_needs,
                // Validated at save against the keys the uploader declared, so a
                // node never receives a key the component does not read.
                "config": p.config,
                "secrets": p.secrets
                    .iter()
                    .map(|(key, r)| json!({ "key": key, "ref": r }))
                    .collect::<Vec<_>>(),
                // Stamped, never authored. A tenant that could write this could
                // write its own way off the box.
                "egress": if is_root || input.strategy == Strategy::Fused { egress.clone() } else { vec![] },
                "pool_size": pool,
                "max_invocations": input.plan.max_invocations,
            })
        })
        .collect();

    let links: Vec<Value> = if input.strategy == Strategy::Fused {
        // `wac plug` erased these at build time. They stay in the manifest anyway:
        // they are the build recipe, and without them a revision cannot be rebuilt
        // or diffed against its successor.
        input.edges.iter().map(|(p, s, i)| json!({ "plug": p, "socket": s, "iface": i })).collect()
    } else {
        input.edges.iter().map(|(p, s, i)| json!({ "plug": p, "socket": s, "iface": i })).collect()
    };

    Ok(json!({
        "app": input.name,
        "tenant": input.tenant,
        "strategy": input.strategy.as_str(),
        "components": components,
        "links": links,
        "ingress": { "host": input.ingress_host, "component": input.root },
        // Not read by the reconciler; here so a stored revision is self-describing
        // when someone opens it at 3am.
        "env": env_for(input.tenant, input.name),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(name: &str, digest: &str) -> Part {
        Part {
            name: name.into(),
            config: Default::default(),
            secrets: Vec::new(),
            digest: digest.into(),
            host_imports: vec![],
            nested_instances: 1,
            serves_http: true,
        }
    }

    fn input<'a>(parts: &'a [Part], plan: &'a Plan, edges: &'a [(String, String, String)]) -> ManifestInput<'a> {
        ManifestInput {
            tenant: "alice",
            name: "shop",
            strategy: Strategy::Linked,
            parts,
            plan,
            edges,
            root: "api",
            ingress_host: "shop.alice.example.com",
        }
    }

    #[test]
    fn a_tag_is_refused_because_it_cannot_be_re_derived() {
        // ADR-0006, and the reason the whole platform speaks digests.
        let parts = vec![part("api", "registry.example/api:latest")];
        let plan = Plan::default();
        let err = build(&input(&parts, &plan, &[])).unwrap_err();
        assert_eq!(err, ManifestError::NotDigestPinned("api".into()));
        assert!(err.detail().contains("has not been distributed"), "{}", err.detail());
    }

    #[test]
    fn egress_is_emitted_bare_and_port_qualified() {
        // The jobs.yaml lesson: egress is fail-closed, so a missing form is a
        // mystery at runtime rather than an error at deploy.
        let parts = vec![part("api", "sha256:a")];
        let plan = Plan { egress: vec!["api.stripe.com".into()], ..Plan::default() };
        let m = build(&input(&parts, &plan, &[])).unwrap();
        let e = m["components"][0]["egress"].as_array().unwrap();
        let got: Vec<&str> = e.iter().filter_map(|v| v.as_str()).collect();
        assert!(got.contains(&"api.stripe.com"));
        assert!(got.contains(&"api.stripe.com:443"));
        assert!(got.contains(&"api.stripe.com:80"));
    }

    #[test]
    fn a_scheme_qualified_egress_entry_is_not_split_on_its_colon() {
        // Splitting this would allow-list the host `https`, which is legal and
        // would be a very quiet hole.
        assert_eq!(expand_egress(&["https://api.example.com".into()]), vec!["https://api.example.com"]);
    }

    #[test]
    fn only_schedulable_host_imports_become_host_needs() {
        // Ambient WASI must not land in host_needs: a node does not advertise
        // `wasi:cli`, so every deployment would be permanently unschedulable.
        let mut p = part("api", "sha256:a");
        p.host_imports = vec![
            HostIface { namespace: "wasi".into(), pkg: "keyvalue".into(), iface: "store".into() },
            HostIface { namespace: "wasi".into(), pkg: "cli".into(), iface: "environment".into() },
            HostIface { namespace: "wasi".into(), pkg: "http".into(), iface: "types".into() },
        ];
        let parts = vec![p];
        let plan = Plan::default();
        let m = build(&input(&parts, &plan, &[])).unwrap();
        let needs: Vec<&str> =
            m["components"][0]["host_needs"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(needs, vec!["wasi:keyvalue/store"]);
    }

    #[test]
    fn pool_size_is_clamped_against_the_core_instance_budget() {
        // ADR-0008's starvation: 48 × 28 = 1344 against a 1000 cap.
        let mut p = part("api", "sha256:a");
        p.nested_instances = 28;
        let parts = vec![p];
        let plan = Plan { pool_size: 48, ..Plan::default() };
        let m = build(&input(&parts, &plan, &[])).unwrap();
        assert_eq!(m["components"][0]["pool_size"], json!(28));
    }

    #[test]
    fn fused_must_arrive_already_composed() {
        let parts = vec![part("api", "sha256:a"), part("store", "sha256:b")];
        let plan = Plan::default();
        let mut i = input(&parts, &plan, &[]);
        i.strategy = Strategy::Fused;
        assert_eq!(build(&i).unwrap_err(), ManifestError::FusedNotComposed(2));
    }

    #[test]
    fn only_the_entry_point_is_replicated() {
        let parts = vec![part("api", "sha256:a"), part("store", "sha256:b")];
        let plan = Plan { replicas: 3, ..Plan::default() };
        let m = build(&input(&parts, &plan, &[])).unwrap();
        assert_eq!(m["components"][0]["replicas"], json!(3), "the root");
        assert_eq!(m["components"][1]["replicas"], json!(1), "a plug rides along");
    }

    #[test]
    fn links_survive_a_fuse_because_they_are_the_build_recipe() {
        let parts = vec![part("api", "sha256:fused")];
        let plan = Plan::default();
        let edges = vec![("store".to_string(), "api".to_string(), "records:store/store@0.1.0".to_string())];
        let mut i = input(&parts, &plan, &edges);
        i.strategy = Strategy::Fused;
        let m = build(&i).unwrap();
        assert_eq!(m["links"].as_array().unwrap().len(), 1);
        assert_eq!(m["links"][0]["plug"], json!("store"));
    }

    #[test]
    fn the_manifest_matches_what_the_reconciler_parses() {
        // The two crates cannot share a type — one is wasm32, one is native — so
        // the wire shape is the contract. A field renamed on either side breaks
        // deployment silently, which is what this guards.
        let parts = vec![part("api", "sha256:a")];
        let plan = Plan::default();
        let m = build(&input(&parts, &plan, &[])).unwrap();
        for k in ["app", "tenant", "strategy", "components", "links", "ingress"] {
            assert!(!m[k].is_null(), "missing top-level `{k}`");
        }
        for k in ["id", "digest", "replicas", "placement", "host_needs", "config", "secrets", "egress"] {
            assert!(!m["components"][0][k].is_null(), "missing component `{k}`");
        }
        assert_eq!(m["strategy"], json!("linked"));
        assert_eq!(m["ingress"]["component"], json!("api"));
        assert_eq!(m["components"][0]["placement"]["mode"], json!("spread"));
    }

    #[test]
    fn constraints_ride_from_the_plan_into_placement() {
        // The multiregion knob: a tenant's plan can pin them to a jurisdiction and
        // the reconciler will refuse to place them anywhere else.
        let parts = vec![part("api", "sha256:a")];
        let plan = Plan { constraints: vec![("region".into(), "eu-central".into())], ..Plan::default() };
        let m = build(&input(&parts, &plan, &[])).unwrap();
        assert_eq!(m["components"][0]["placement"]["constraints"]["region"], json!("eu-central"));
    }

    #[test]
    fn a_name_that_is_not_a_dns_label_is_refused() {
        let parts = vec![part("api", "sha256:a")];
        let plan = Plan::default();
        let mut i = input(&parts, &plan, &[]);
        i.name = "Shop Inc.";
        assert!(matches!(build(&i).unwrap_err(), ManifestError::BadName(_)));
    }

    #[test]
    fn an_app_identity_is_derived_and_capped() {
        assert_eq!(env_for("alice", "shop"), "app-alice-shop");
        let long = env_for(&"t".repeat(80), &"a".repeat(80));
        assert!(long.len() <= 53 && !long.ends_with('-'), "{long}");
    }
}
