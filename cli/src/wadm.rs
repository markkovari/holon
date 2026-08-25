//! The wasmCloud lanes: one app spec, four manifests.
//!
//! `examples/vet-clinic-wasmcloud/gen-manifest.py` did this for one app, with a
//! `CAPS` table typed out by hand and a `LATTICE=1` flag to switch topology. This is
//! that generator generalised over `apps/<name>.toml` and moved into the CLI, where
//! it can be tested without a cluster.
//!
//! ## Two axes, and both are real
//!
//! **Topology — fused or linked.** `Strategy` already names this (ADR-0005), and it
//! is not a style choice:
//!
//! * `fused` is one artifact from `wac plug`, one component in the manifest, and no
//!   hop between capabilities. It is the faster shape and the simpler manifest.
//! * `linked` is every capability as its own component wired by wadm `link` traits.
//!   It is the shape that DEPLOYS when fusing cannot: wasmtime allows 30 nested
//!   instances per component, and the full vet-clinic is 104 core modules. That is a
//!   measured wall, recorded in the generator this replaces, not a preference.
//!
//! The hop is not free, and the number depends on which wasmCloud you are on:
//!
//! | | per component boundary |
//! |---|---|
//! | comp's own lattice (wrpc, ADR-0032) | 57 µs |
//! | wasmCloud v1 (wadm links over NATS) | **1.2 ms** (`docs/SELFHOST.md`) |
//!
//! Quote the microseconds rather than a percentage: ADR-0032's 5.4% was of a
//! do-nothing request, and `docs/CURRENT.md` warns that the ratio is not a platform
//! property. On v1 a linked graph pays roughly twenty times what comp's lattice
//! charges, which is why `hybrid` exists — fuse the pure-compute capabilities, link
//! only the stateful ones, and pay the hop only where state forces it.
//!
//! **API version — v1 or v2.** wadm's manifest schema changed, and a cluster runs
//! one or the other:
//!
//! * `v1` is `core.oam.dev/v1beta1` with `spreadscaler` and `link` traits. It is
//!   what wadm 0.21 speaks and what the operator 0.4.0 reconciles.
//! * `v2` is the newer `wasmcloud.dev/v1alpha1` shape.
//!
//! This is a property of the CLUSTER, never of the app, so it is a flag rather than
//! a field in the spec: the same app deploys to both.
//!
//! **Getting that flag wrong fails SILENTLY, which is why the renderer stamps it.**
//! Measured against a real wadm 0.21: a v2 manifest was accepted by `model.put`,
//! deployed with `"result":"acknowledged"`, and produced NO scalers at all — wadm
//! ignores a trait type it does not know rather than refusing it. The links still
//! reconciled, so the app looked deployed and ran zero instances. A `holon`
//! annotation in the metadata is what lets `holon wadm check` say which version a
//! stored manifest was rendered for, since the manifest itself cannot be trusted to
//! fail loudly.
//!
//! ## What is NOT here
//!
//! No `wash` invocation. wash 2.x removed `wash app put`, which is what the existing
//! `wadm.yaml` header still instructs — but the API underneath it is a set of NATS
//! subjects (`wadm.api.<lattice>.model.put`) that both versions still serve, and
//! those are what the deploy recipe uses. A renderer that shelled out to a CLI whose
//! command set moves would be the fragile half of this.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::Deserialize;

use crate::Spec;

/// One interface, as `comp-capgraph --format json` reports it.
///
/// The capability graph is DERIVED from the built artifacts — it reads what a
/// component actually imports out of the binary (ADR-0087), so it cannot drift from
/// the code the way a hand-maintained table does. `gen-manifest.py` typed its edges
/// out by hand; this reads them.
#[derive(Debug, Deserialize)]
pub struct Iface {
    /// Fully qualified: `records:store/store@0.1.0`.
    pub interface: String,
    /// The component that exports it.
    pub provider: String,
    /// The components that import it.
    pub consumers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Graph {
    pub interfaces: Vec<Iface>,
}

impl Graph {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the capability graph at {}", path.display()))?;
        serde_json::from_str(&text).context("parsing the capability graph")
    }

    /// Every edge `consumer -> provider`, with the WIT coordinates wadm needs.
    ///
    /// A link trait carries `namespace`, `package` and `interfaces` — a target alone
    /// is refused ("Link trait deserialized as custom trait"), because wadm has no
    /// way to know WHICH import of the consumer this satisfies. Those three fields
    /// are exactly what the interface string already spells.
    fn edges_from(&self, consumer: &str) -> Vec<Edge> {
        let mut by_target: BTreeMap<(String, String, String), Vec<String>> = BTreeMap::new();
        for i in &self.interfaces {
            if !i.consumers.iter().any(|c| c == consumer) {
                continue;
            }
            let Some((ns, package, iface)) = split_wit(&i.interface) else { continue };
            by_target.entry((i.provider.clone(), ns, package)).or_default().push(iface);
        }
        by_target
            .into_iter()
            .map(|((target, namespace, package), mut interfaces)| {
                interfaces.sort();
                interfaces.dedup();
                Edge { target, namespace, package, interfaces }
            })
            .collect()
    }
}

pub struct Edge {
    pub target: String,
    pub namespace: String,
    pub package: String,
    pub interfaces: Vec<String>,
}

/// `records:store/store@0.1.0` -> ("records", "store", "store").
///
/// The version is dropped on purpose: wadm links name the package, and a version in
/// that field is what makes a link silently match nothing.
fn split_wit(s: &str) -> Option<(String, String, String)> {
    let (head, iface) = s.split_once('/')?;
    let (ns, package) = head.split_once(':')?;
    let iface = iface.split('@').next()?;
    Some((ns.to_string(), package.to_string(), iface.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Topology {
    /// One `wac`-composed artifact. No hop, and the simplest manifest — but it
    /// cannot exceed wasmtime's 30 nested instances per component.
    Fused,
    /// Every capability its own component, wired by wadm links. Deploys at any size;
    /// pays a hop per boundary.
    Linked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApiVersion {
    /// `core.oam.dev/v1beta1` — wadm 0.21, operator 0.4.x.
    V1,
    /// `wasmcloud.dev/v1alpha1` — the newer manifest schema.
    V2,
}

impl ApiVersion {
    fn api(&self) -> &'static str {
        match self {
            ApiVersion::V1 => "core.oam.dev/v1beta1",
            ApiVersion::V2 => "wasmcloud.dev/v1alpha1",
        }
    }
    /// v1 spells the scaler `spreadscaler`; v2 renamed it.
    fn scaler(&self) -> &'static str {
        match self {
            ApiVersion::V1 => "spreadscaler",
            ApiVersion::V2 => "scaler",
        }
    }
}

/// Everything the manifest needs that the app spec does not carry, because it
/// describes the CLUSTER rather than the app.
pub struct Target {
    /// Registry the artifacts are pushed to and the host pulls from.
    pub registry: String,
    /// In-cluster NATS, for the keyvalue provider.
    pub nats: String,
    /// Where the http-server provider listens.
    pub addr: String,
    /// Replicas of the HTTP-facing component.
    pub replicas: u32,
}

impl Default for Target {
    fn default() -> Self {
        Target {
            registry: "registry.wasmcloud.svc.cluster.local:5000".into(),
            nats: "nats://nats.wasmcloud.svc.cluster.local:4222".into(),
            addr: "0.0.0.0:8080".into(),
            replicas: 1,
        }
    }
}

/// Which capabilities keep state, and therefore need a `wasi:keyvalue` link.
///
/// Derived from the component's own name rather than a hand-typed table: a pure
/// compute capability that later grows a store would otherwise be linked to nothing
/// and fail at its first write. Conservative on purpose — an unnecessary link costs
/// a bucket nobody writes to, while a missing one costs a runtime trap.
fn stateful(component: &str) -> bool {
    const PURE: &[&str] = &[
        "money", "validate", "markdown", "pii-redact", "pagination", "upload-policy",
        "csv", "diff", "semver-range", "shaper", "cron", "geo", "i18n-catalog",
    ];
    !PURE.contains(&component)
}

/// The capabilities worth fusing into the root even in a linked deployment.
///
/// Pure compute, so fusing them costs nothing but saves a hop each — and on v1 that
/// hop is 1.2 ms. This is `gen-manifest.py`'s `FUSED` set, derived instead of typed.
pub fn fusable(components: &[String]) -> Vec<&str> {
    components.iter().map(|s| s.as_str()).filter(|c| !stateful(c)).collect()
}

fn dns(s: &str) -> String {
    s.replace('_', "-")
}

/// Render the wadm Application for one app.
pub fn render(
    spec: &Spec,
    topo: Topology,
    api: ApiVersion,
    t: &Target,
    graph: Option<&Graph>,
) -> Result<String> {
    if topo == Topology::Linked && spec.components.is_empty() {
        bail!(
            "`{}` has no `components` in its spec, so there is nothing to link — \
             list them, or render --topology fused",
            spec.name
        );
    }

    let root = format!("{}-domain", spec.name);
    let mut y = String::new();
    y.push_str(&format!(
        "# Generated by `holon wadm render` — do not edit; edit apps/{}.toml.\n#\n",
        spec.name
    ));
    match topo {
        Topology::Fused => y.push_str(
            "# FUSED topology: one wac-composed artifact, one component, no hop between\n\
             # capabilities. Refused by the host above 30 nested instances per component —\n\
             # render --topology linked when a graph outgrows that.\n",
        ),
        Topology::Linked => y.push_str(
            "# LINKED topology: every capability a separate component wired by wadm links.\n\
             # Deploys at any size, and pays a hop per boundary — 57us on comp's own\n\
             # lattice (ADR-0032), ~1.2ms on wasmCloud v1, where links go over NATS.\n",
        ),
    }
    y.push_str(&format!(
        "apiVersion: {}\nkind: Application\nmetadata:\n  name: {}\n  annotations:\n    \
         description: \"{} on wasmCloud ({:?}, {:?})\"\n    \
         holon.dev/api: \"{:?}\"\n    holon.dev/topology: \"{:?}\"\nspec:\n  components:\n",
        api.api(),
        dns(&spec.name),
        spec.name,
        topo,
        api,
        api,
        topo
    ));

    // ---- the root component ------------------------------------------------
    y.push_str(&format!(
        "    - name: {}\n      type: component\n      properties:\n        image: oci://{}/{}:latest\n",
        dns(&root),
        t.registry,
        dns(&spec.name)
    ));
    y.push_str(&format!(
        "      traits:\n        - type: {}\n          properties:\n            instances: {}\n",
        api.scaler(),
        t.replicas
    ));

    if topo == Topology::Linked {
        let fused = fusable(&spec.components);
        // The edges, with their WIT coordinates. A link naming only a target is
        // refused by wadm — it cannot know which import of the consumer it
        // satisfies — so a linked render needs the graph.
        let edges = match graph {
            Some(g) => g.edges_from(&root),
            None => bail!(
                "a linked render needs the capability graph for `{}`'s edges: \n                   comp-capgraph --format json > graph.json && holon wadm render ... --graph graph.json\n\n                 wadm refuses a link that names only a target, because the WIT namespace, \n                 package and interfaces are what say WHICH import it satisfies.",
                root
            ),
        };
        for e in &edges {
            if fused.contains(&e.target.as_str()) {
                continue;
            }
            y.push_str(&format!(
                "        - type: link\n          properties:\n            namespace: {}\n            \
                 package: {}\n            interfaces: [{}]\n            target:\n              name: {}\n",
                e.namespace,
                e.package,
                e.interfaces.join(", "),
                dns(&e.target)
            ));
        }
    }
    // The app reads its `wasi:config` keys the same way in every lane; only the
    // delivery differs (CFG_* env in tier 1, this table here).
    if !spec.config.is_empty() {
        y.push_str("      properties_config:\n");
        for (k, v) in &spec.config {
            y.push_str(&format!("        {k}: \"{v}\"\n"));
        }
    }

    // ---- the capabilities, when they are separate components ---------------
    if topo == Topology::Linked {
        let fused: Vec<&str> = fusable(&spec.components);
        let linked: Vec<String> = match graph {
            Some(g) => g.edges_from(&root).into_iter().map(|e| e.target).collect(),
            None => Vec::new(),
        };
        for c in &linked {
            if c == &root || fused.contains(&c.as_str()) {
                continue;
            }
            y.push_str(&format!(
                "    - name: {}\n      type: component\n      properties:\n        image: oci://{}/{}:latest\n      traits:\n        - type: {}\n          properties:\n            instances: 1\n",
                dns(c),
                t.registry,
                dns(c),
                api.scaler()
            ));
            if stateful(c) {
                y.push_str(&kv_link(c, &t.nats));
            }
        }
    }

    // ---- the keyvalue provider ---------------------------------------------
    // wadm refuses a manifest whose link names a capability the manifest does not
    // declare ("The following capability component(s) are missing"), so a target is
    // not enough — the provider is a component like any other. Emitted only when
    // something actually links to it, so a stateless app carries no NATS provider.
    let links_state = graph
        .map(|g| g.edges_from(&root).iter().any(|e| e.target != root && stateful(&e.target)))
        .unwrap_or(false);
    if topo == Topology::Linked && links_state {
        y.push_str(
            "    - name: keyvalue-nats\n      type: capability\n      properties:\n        \
             image: ghcr.io/wasmcloud/keyvalue-nats:0.3.1\n",
        );
    }

    // ---- the door ----------------------------------------------------------
    // An http-server provider, not the app listening itself: on wasmCloud the
    // component exports wasi:http/incoming-handler and a provider brings the socket.
    y.push_str(&format!(
        "    - name: httpserver\n      type: capability\n      properties:\n        \
         image: ghcr.io/wasmcloud/http-server:0.23.1\n      traits:\n        \
         - type: link\n          properties:\n            target:\n              name: {}\n            \
         namespace: wasi\n            package: http\n            interfaces: [incoming-handler]\n            \
         source_config:\n              - name: {}-http\n                properties:\n                  \
         address: {}\n",
        dns(&root),
        dns(&spec.name),
        t.addr
    ));

    Ok(y)
}

/// The `wasi:keyvalue` link every stateful capability needs, each to its own bucket.
///
/// A bucket per component, not one shared: `docs/SELFHOST.md` makes the same point
/// for tier 1, and ADR-0015 is the general rule — a bucket name is not a boundary,
/// so two components sharing one is two components sharing state.
fn kv_link(component: &str, nats: &str) -> String {
    format!(
        "        - type: link\n          properties:\n            namespace: wasi\n            \
         package: keyvalue\n            interfaces: [store, atomics]\n            target:\n              \
         name: keyvalue-nats\n              config:\n                - name: {c}-bucket\n                  \
         properties:\n                    bucket: {c}\n                    cluster_uri: {nats}\n                    \
         enable_bucket_auto_create: \"true\"\n",
        c = dns(component),
        nats = nats
    )
}

/// The operator's host, for the Kubernetes lane.
///
/// The wadm Application above is IDENTICAL whether wadm is driven by `wash` or by
/// the operator — which is what makes the k8s lane small. This is the only file that
/// is specific to it.
pub fn render_host_config(namespace: &str, lattice: &str, version: &str, t: &Target) -> String {
    format!(
        "# Generated by `holon wadm host` — do not edit.\n#\n\
         # The operator reconciles this into a host Deployment. The Application manifest\n\
         # beside it is the same one `wash` would deploy: only the DRIVER differs.\n\
         #\n\
         # ADR-0021 took Kubernetes off this platform's runtime path deliberately, and\n\
         # priced it — 70 Mi per pod against 2.3 Mi per extra component, with a control\n\
         # plane of ~800 Mi-1 GB before any app exists. This lane is for running Holon\n\
         # apps on a cluster somebody ELSE already operates, not a recommendation to\n\
         # stand one up. Start at tier 1 (docs/SELFHOST.md).\n\
         apiVersion: k8s.wasmcloud.dev/v1alpha1\nkind: WasmCloudHostConfig\nmetadata:\n  \
         name: {lattice}-host\n  namespace: {namespace}\nspec:\n  hostReplicas: 1\n  \
         lattice: {lattice}\n  version: \"{version}\"\n  natsAddress: {nats}\n  \
         natsClientPort: 4222\n  jetstreamDomain: default\n  logLevel: info\n  \
         allowLatest: true\n  allowedInsecure:\n    - {registry}\n",
        lattice = lattice,
        namespace = namespace,
        version = version,
        nats = t.nats.trim_end_matches(":4222"),
        registry = t.registry
    )
}

/// Refuse a fused render that the host would reject at start.
///
/// wasmtime allows 30 nested instances per component. `wac` will happily produce the
/// artifact anyway, so without this the failure arrives as a start-time trap on the
/// cluster rather than as a sentence here — which is the whole reason the linked
/// manifest exists in the tree at all.
pub fn check_fusable(spec: &Spec, topo: Topology) -> Result<()> {
    const CEILING: usize = 30;
    if topo == Topology::Fused && spec.components.len() > CEILING {
        bail!(
            "`{}` lists {} components, over wasmtime's {CEILING} nested instances per component — \
             it would build and then fail to start. Render --topology linked.",
            spec.name,
            spec.components.len()
        );
    }
    Ok(())
}

/// Config that belongs to the whole deployment rather than one component.
pub type Overrides = BTreeMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The real edges `comp-capgraph` reports for `gate-domain`, as a fixture.
    fn graph() -> Graph {
        serde_json::from_str(
            r#"{"interfaces":[
              {"interface":"records:store/store@0.1.0","provider":"record-store",
               "consumers":["gate-domain"]},
              {"interface":"shaper:limit/limiter@0.1.0","provider":"shaper",
               "consumers":["gate-domain"]},
              {"interface":"session:store/sessions@0.1.0","provider":"session-store",
               "consumers":["gate-domain"]}
            ]}"#,
        )
        .unwrap()
    }

    fn spec(extra: &str) -> Spec {
        toml::from_str(&format!(
            "name = \"gate\"\ndomain = \"gate.example.com\"\nartifact = \"a.wasm\"\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn a_fused_render_is_one_component_and_no_links() {
        let s = spec("components = [\"gate-domain\", \"record-store\", \"shaper\"]\n");
        let y = render(&s, Topology::Fused, ApiVersion::V1, &Target::default(), None).unwrap();
        assert!(y.contains("apiVersion: core.oam.dev/v1beta1"), "{y}");
        // The capabilities are INSIDE the artifact, so they must not appear.
        assert!(!y.contains("name: record-store"), "fused hides its parts: {y}");
        assert!(y.contains("name: gate-domain"), "{y}");
        // Still needs a door.
        assert!(y.contains("http-server"), "{y}");
    }

    #[test]
    fn a_linked_render_names_every_stateful_capability() {
        let s = spec("components = [\"gate-domain\", \"record-store\", \"shaper\"]\n");
        let y = render(&s, Topology::Linked, ApiVersion::V1, &Target::default(), Some(&graph())).unwrap();
        assert!(y.contains("name: record-store"), "{y}");
        // record-store keeps state, so it gets its own bucket...
        assert!(y.contains("bucket: record-store"), "{y}");
        // ...and shaper is pure compute, so it is fused in rather than linked.
        assert!(!y.contains("name: shaper"), "pure compute should fuse: {y}");
    }

    #[test]
    fn the_keyvalue_provider_is_declared_and_not_merely_targeted() {
        // wadm rejects a link naming a capability the manifest does not declare.
        // Found against a real wadm 0.21, which answered:
        //   "The following capability component(s) are missing from the manifest"
        let s = spec("components = [\"gate-domain\", \"record-store\"]\n");
        let y = render(&s, Topology::Linked, ApiVersion::V1, &Target::default(), Some(&graph())).unwrap();
        assert!(y.contains("name: keyvalue-nats"), "{y}");
        assert!(y.contains("image: ghcr.io/wasmcloud/keyvalue-nats"), "{y}");

        // ...and an app whose edges reach nothing stateful carries no provider it
        // never calls. The GRAPH decides that, not the spec's component list: the
        // edges are read from the built artifacts, and the list is a hint for the
        // fused set.
        let pure_graph: Graph = serde_json::from_str(
            r#"{"interfaces":[{"interface":"shaper:limit/limiter@0.1.0",
                 "provider":"shaper","consumers":["gate-domain"]}]}"#,
        )
        .unwrap();
        let pure = spec("components = [\"gate-domain\", \"shaper\"]\n");
        let y2 =
            render(&pure, Topology::Linked, ApiVersion::V1, &Target::default(), Some(&pure_graph))
                .unwrap();
        assert!(!y2.contains("keyvalue-nats"), "{y2}");
    }

    #[test]
    fn each_stateful_capability_gets_its_own_bucket() {
        // ADR-0015: a bucket name is not a boundary, so sharing one is sharing state.
        let s = spec("components = [\"gate-domain\", \"record-store\", \"session-store\"]\n");
        // Both are stateful and both appear as edges in the fixture graph.
        let y = render(&s, Topology::Linked, ApiVersion::V1, &Target::default(), Some(&graph())).unwrap();
        assert!(y.contains("bucket: record-store"), "{y}");
        assert!(y.contains("bucket: session-store"), "{y}");
    }

    #[test]
    fn the_rendered_api_version_is_stamped_because_wadm_will_not_complain() {
        // A real wadm 0.21 accepted a v2 manifest, deployed it, and silently created
        // no scalers — it ignores an unknown trait type. So "which version is this"
        // has to be answerable from the file rather than from the deploy result.
        let s = spec("components = [\"gate-domain\", \"record-store\"]\n");
        let t = Target::default();
        let v1 = render(&s, Topology::Fused, ApiVersion::V1, &t, None).unwrap();
        let v2 = render(&s, Topology::Fused, ApiVersion::V2, &t, None).unwrap();
        assert!(v1.contains("holon.dev/api: \"V1\""), "{v1}");
        assert!(v2.contains("holon.dev/api: \"V2\""), "{v2}");
        assert!(v1.contains("holon.dev/topology: \"Fused\""), "{v1}");
    }

    #[test]
    fn v1_and_v2_differ_in_the_envelope_and_not_in_the_app() {
        let s = spec("components = [\"gate-domain\", \"record-store\"]\n");
        let t = Target::default();
        let v1 = render(&s, Topology::Linked, ApiVersion::V1, &t, Some(&graph())).unwrap();
        let v2 = render(&s, Topology::Linked, ApiVersion::V2, &t, Some(&graph())).unwrap();
        assert!(v1.contains("core.oam.dev/v1beta1") && v1.contains("type: spreadscaler"), "{v1}");
        assert!(v2.contains("wasmcloud.dev/v1alpha1") && v2.contains("type: scaler"), "{v2}");
        // The app is the same on both: same image, same capability, same bucket.
        for y in [&v1, &v2] {
            assert!(y.contains("name: record-store") && y.contains("bucket: record-store"));
        }
    }

    #[test]
    fn a_graph_too_big_to_fuse_is_refused_here_rather_than_on_the_cluster() {
        // wac builds it; wasmtime refuses it at start. Saying so now is the whole
        // reason the linked manifest exists (gen-manifest.py: 104 core modules).
        let many: Vec<String> = (0..31).map(|i| format!("\"cap-{i}\"")).collect();
        let s = spec(&format!("components = [{}]\n", many.join(", ")));
        let err = check_fusable(&s, Topology::Fused).unwrap_err().to_string();
        assert!(err.contains("nested instances"), "{err}");
        // The same graph is fine linked — that is the escape hatch.
        assert!(check_fusable(&s, Topology::Linked).is_ok());
    }

    #[test]
    fn linked_with_no_component_list_says_so_instead_of_emitting_a_lone_root() {
        let s = spec("");
        let err = render(&s, Topology::Linked, ApiVersion::V1, &Target::default(), Some(&graph()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("nothing to link"), "{err}");
    }

    #[test]
    fn the_host_config_says_why_this_lane_is_not_the_default() {
        // ADR-0021 removed Kubernetes on purpose. A generated file that did not say
        // so would quietly contradict the tree.
        let h = render_host_config("holon", "prod", "1.6.0", &Target::default());
        assert!(h.contains("k8s.wasmcloud.dev/v1alpha1"), "{h}");
        assert!(h.contains("ADR-0021"), "{h}");
        assert!(h.contains("lattice: prod"), "{h}");
    }
}
