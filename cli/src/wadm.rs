//! The wasmCloud lanes: one app spec, four manifests (fused/linked x v1/v2).
use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::Deserialize;

use crate::selfhost::{Daemon, Spec};

/// As `comp-capgraph --format json` reports it, derived from built artifacts (ADR-0087).
#[derive(Debug, Deserialize)]
pub struct Iface {
    /// Fully qualified: `records:store/store@0.1.0`.
    pub interface: String,
    pub provider: String,
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

    /// Every edge from `consumer`, with the namespace/package/interfaces a wadm link needs:
    /// a target alone is refused, since wadm can't tell which import it satisfies.
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
/// The version is dropped: a versioned package in a wadm link silently matches nothing.
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

/// Different kinds of document: v1 is an OAM `Application` for wadm over NATS, v2 a Kubernetes
/// `Workload`. wadm 0.21 silently accepts a bogus v2 OAM manifest and creates no scalers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApiVersion {
    /// wasmCloud 1.x: an OAM `Application` for wadm, over NATS.
    ///
    /// Verified against wadm 0.21.0 + operator 0.4.0 + wasmCloud 1.6.0.
    V1,
    /// wasmCloud 2.x: a `Workload` for the runtime-operator, over the Kubernetes API.
    ///
    /// Verified against runtime-operator 2.8.0 + wash 2.8.0.
    V2,
}

impl ApiVersion {
    fn api(&self) -> &'static str {
        match self {
            ApiVersion::V1 => "core.oam.dev/v1beta1",
            ApiVersion::V2 => "runtime.wasmcloud.dev/v1alpha1",
        }
    }
    /// v1's scaler trait; v2 has no traits (replicas live on a `WorkloadDeployment`).
    fn scaler(&self) -> &'static str {
        "spreadscaler"
    }
}

/// What the manifest needs that describes the cluster, not the app.
pub struct Target {
    pub registry: String,
    pub nats: String,
    pub addr: String,
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

/// Conservative on purpose: an unneeded link costs an unused bucket, a missing one a runtime trap.
fn stateful(component: &str) -> bool {
    const PURE: &[&str] = &[
        "money",
        "validate",
        "markdown",
        "pii-redact",
        "pagination",
        "upload-policy",
        "csv",
        "diff",
        "semver-range",
        "shaper",
        "cron",
        "geo",
        "i18n-catalog",
    ];
    !PURE.contains(&component)
}

/// Pure compute: fusing costs nothing and saves a hop each (1.2ms on v1).
pub fn fusable(components: &[String]) -> Vec<&str> {
    components.iter().map(|s| s.as_str()).filter(|c| !stateful(c)).collect()
}

fn dns(s: &str) -> String {
    s.replace('_', "-")
}

/// Prefer the spec's first component: not every root is `<app>-domain` (`lan-scanner`), and a
/// wrong guess still deploys, just misleadingly.
fn root_of(spec: &Spec) -> String {
    if let Some(first) = spec.components.first() {
        return first.clone();
    }
    format!("{}-domain", spec.name)
}

/// A wasmCloud 2.x `Workload`, for the Kubernetes API (no wadm). Each rule below was a real
/// failure against runtime-operator 2.8.0 first.
pub fn render_workload(
    spec: &Spec,
    namespace: &str,
    t: &Target,
    graph: Option<&Graph>,
    imports_config: bool,
) -> Result<String> {
    let root = root_of(spec);
    let mut y = String::new();
    y.push_str(&format!(
        "# Generated by `holon wadm render --api v2` — do not edit; edit apps/{}.toml.\n#\n\
         # A wasmCloud 2.x Workload. NOT an OAM manifest: 2.x dropped wadm, so this is\n\
         # applied to the Kubernetes API and the runtime-operator schedules it onto a\n\
         # host in a host group.\n\
         #\n\
         #   kubectl apply -f this.yaml\n\
         #   kubectl get workloads -n {}\n",
        spec.name, namespace
    ));
    y.push_str(&format!(
        "apiVersion: {}\nkind: Workload\nmetadata:\n  name: {}\n  namespace: {}\n  \
         annotations:\n    holon.dev/api: \"V2\"\nspec:\n  components:\n",
        ApiVersion::V2.api(),
        dns(&spec.name),
        namespace
    ));
    // No `oci://`: v1 wants that prefix and v2 rejects it as an invalid reference.
    y.push_str(&format!(
        "    - name: {}\n      image: {}/{}:latest\n",
        dns(&root),
        t.registry,
        dns(&spec.name)
    ));
    // A worker listens on nothing: no Service to grant, no `<name>-url` to rewrite.
    let served: Vec<(&Daemon, &str)> =
        spec.daemons.iter().filter_map(|d| d.addr.as_deref().map(|a| (d, a))).collect();
    if !served.is_empty() {
        // Egress is fail-closed, like comp-host's `--egress`: dialing (`allowedHosts`) and resolving
        // (`allowedIpNameLookups`) are separate grants, both under `localResources` (`kubectl explain`).
        y.push_str("      localResources:\n        allowedHosts:\n");
        for (d, addr) in &served {
            y.push_str(&format!("          - {}\n", daemon_service_host(namespace, d, addr)));
        }
        y.push_str("        allowedIpNameLookups:\n");
        for (d, _) in &served {
            y.push_str(&format!("          - {}\n", daemon_fqdn(namespace, d)));
        }
    }

    // Every non-standard-WASI import must be declared, or the workload fails to link.
    y.push_str("  hostInterfaces:\n");
    y.push_str(&format!(
        "    # The door. The host serves one port and routes by `Host` header, so a\n\
         \x20   # workload without a hostname is refused with `no host header found`.\n\
         \x20   - namespace: wasi\n      package: http\n      interfaces: [incoming-handler]\n      \
         config:\n        host: {}\n",
        spec.domain
    ));

    let mut needs_kv = false;
    if let Some(g) = graph {
        for e in g.edges_from(&root) {
            if stateful(&e.target) {
                needs_kv = true;
            }
        }
    }
    if needs_kv || spec.kv != "memory" {
        y.push_str(
            "    - namespace: wasi\n      package: keyvalue\n      interfaces: [store]\n      \
             version: \"0.2.0-draft\"\n",
        );
    }
    // Gated on the artifact, not `spec.config`: a shared dependency can import `wasi:config/store`
    // with an empty `[config]` (`flags`), and an undeclared import fails to link.
    if imports_config {
        y.push_str(
            "    - namespace: wasi\n      package: config\n      interfaces: [store]\n      \
             version: \"0.2.0-rc.1\"\n      config:\n",
        );
        for (k, v) in &spec.config {
            // `<name>-url` points at tier 1's loopback sibling; here the daemon is its own Service.
            let value = served
                .iter()
                .find(|(d, _)| *k == format!("{}-url", d.name))
                .map(|(d, addr)| format!("http://{}", daemon_service_host(namespace, d, addr)))
                .unwrap_or_else(|| v.clone());
            y.push_str(&format!("        {k}: \"{value}\"\n"));
        }
    }
    for d in &spec.daemons {
        y.push_str("---\n");
        y.push_str(&render_daemon_deployment(d, namespace, &t.registry));
    }
    Ok(y)
}

fn addr_port(addr: &str) -> &str {
    addr.rsplit_once(':').map(|(_, p)| p).unwrap_or(addr)
}

fn daemon_service_name(d: &Daemon) -> String {
    format!("{}-daemon", dns(&d.name))
}

/// No port: what `allowedIpNameLookups` grants resolving.
fn daemon_fqdn(namespace: &str, d: &Daemon) -> String {
    format!("{}.{namespace}.svc.cluster.local", daemon_service_name(d))
}

fn daemon_service_host(namespace: &str, d: &Daemon, addr: &str) -> String {
    format!("{}:{}", daemon_fqdn(namespace, d), addr_port(addr))
}

/// A `[[daemon]]`'s Deployment + Service (a worker gets no Service), from `reconciler/Dockerfile.daemon`.
/// No token support yet: wire a Secret + `--token-file` before sharing a namespace.
fn render_daemon_deployment(d: &Daemon, namespace: &str, registry: &str) -> String {
    let name = daemon_service_name(d);
    let mut args = String::new();
    if let Some(addr) = &d.addr {
        let port = addr_port(addr);
        args.push_str(&format!("            - --addr\n            - \"0.0.0.0:{port}\"\n"));
    }
    if let Some(flag) = &d.allow_flag {
        for v in &d.allow {
            args.push_str(&format!("            - --{flag}\n            - \"{v}\"\n"));
        }
    }
    for a in &d.extra_args {
        for part in a.split_whitespace() {
            args.push_str(&format!("            - \"{part}\"\n"));
        }
    }
    let deployment = format!(
        "apiVersion: apps/v1\n\
         kind: Deployment\n\
         metadata:\n\
         \x20 name: {name}\n\
         \x20 namespace: {namespace}\n\
         spec:\n\
         \x20 replicas: 1\n\
         \x20 selector:\n\
         \x20   matchLabels:\n\
         \x20     app: {name}\n\
         \x20 template:\n\
         \x20   metadata:\n\
         \x20     labels:\n\
         \x20       app: {name}\n\
         \x20   spec:\n\
         \x20     containers:\n\
         \x20       - name: daemon\n\
         \x20         image: {registry}/comp-{dname}:latest\n\
         \x20         args:\n\
         {args}",
        dname = dns(&d.name),
    );
    let Some(addr) = &d.addr else { return deployment };
    let port = addr_port(addr);
    format!(
        "{deployment}---\n\
         apiVersion: v1\n\
         kind: Service\n\
         metadata:\n\
         \x20 name: {name}\n\
         \x20 namespace: {namespace}\n\
         spec:\n\
         \x20 selector:\n\
         \x20   app: {name}\n\
         \x20 ports:\n\
         \x20   - port: {port}\n\
         \x20     targetPort: {port}\n"
    )
}

/// Imports a 2.x release host can't satisfy: it provides standard WASI and `wasmcloud:messaging`,
/// the rest needs host plugins release images lack. Read from the binary: the graph has no `comp:` imports.
pub fn unsupported_on_v2(artifact: &std::path::Path) -> Result<Vec<String>> {
    const SATISFIABLE: &[&str] = &["wasi:", "wasmcloud:"];

    let mut bad: Vec<String> = component_imports(artifact)?
        .into_iter()
        .filter(|i| !SATISFIABLE.iter().any(|ok| i.starts_with(ok)))
        .collect();
    bad.sort();
    bad.dedup();
    Ok(bad)
}

/// Read from the binary: a shared dependency can import it with an empty `[config]` (`flags`).
pub fn imports_wasi_config(artifact: &std::path::Path) -> Result<bool> {
    Ok(component_imports(artifact)?.iter().any(|i| i.starts_with("wasi:config/store")))
}

fn component_imports(artifact: &std::path::Path) -> Result<Vec<String>> {
    let out = std::process::Command::new("wasm-tools")
        .args(["component", "wit"])
        .arg(artifact)
        .output()
        .with_context(|| format!("running wasm-tools on {}", artifact.display()))?;
    if !out.status.success() {
        bail!(
            "wasm-tools could not read {}: {}",
            artifact.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let wit = String::from_utf8_lossy(&out.stdout);
    Ok(wit
        .lines()
        .filter_map(|l| l.trim().strip_prefix("import "))
        .map(|l| l.trim_end_matches(';').to_string())
        .collect())
}

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

    let root = root_of(spec);
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
        // wadm refuses a link naming only a target, so a linked render needs the graph.
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
    if !spec.config.is_empty() {
        y.push_str("      properties_config:\n");
        for (k, v) in &spec.config {
            y.push_str(&format!("        {k}: \"{v}\"\n"));
        }
    }

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

    // wadm refuses a link to an undeclared capability, so declare the provider, but only when
    // something links to it.
    let links_state = graph
        .map(|g| g.edges_from(&root).iter().any(|e| e.target != root && stateful(&e.target)))
        .unwrap_or(false);
    if topo == Topology::Linked && links_state {
        y.push_str(
            "    - name: keyvalue-nats\n      type: capability\n      properties:\n        \
             image: ghcr.io/wasmcloud/keyvalue-nats:0.3.1\n",
        );
    }

    // On wasmCloud a provider brings the socket; the component only exports incoming-handler.
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

/// A bucket per component: a bucket name is not a boundary (ADR-0015).
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

/// The only k8s-specific file: the Application is identical whether `wash` or the operator drives wadm.
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

/// wasmtime allows 30 nested instances per component and `wac` builds past that anyway,
/// so refuse here rather than as a start-time trap on the cluster.
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!y.contains("name: record-store"), "fused hides its parts: {y}");
        assert!(y.contains("name: gate-domain"), "{y}");
        assert!(y.contains("http-server"), "{y}");
    }

    #[test]
    fn a_linked_render_names_every_stateful_capability() {
        let s = spec("components = [\"gate-domain\", \"record-store\", \"shaper\"]\n");
        let y = render(&s, Topology::Linked, ApiVersion::V1, &Target::default(), Some(&graph()))
            .unwrap();
        assert!(y.contains("name: record-store"), "{y}");
        assert!(y.contains("bucket: record-store"), "{y}");
        assert!(!y.contains("name: shaper"), "pure compute should fuse: {y}");
    }

    #[test]
    fn the_keyvalue_provider_is_declared_and_not_merely_targeted() {
        // Found against wadm 0.21: "capability component(s) are missing from the manifest".
        let s = spec("components = [\"gate-domain\", \"record-store\"]\n");
        let y = render(&s, Topology::Linked, ApiVersion::V1, &Target::default(), Some(&graph()))
            .unwrap();
        assert!(y.contains("name: keyvalue-nats"), "{y}");
        assert!(y.contains("image: ghcr.io/wasmcloud/keyvalue-nats"), "{y}");

        // The graph, not the component list, decides whether a provider is needed.
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
        let s = spec("components = [\"gate-domain\", \"record-store\", \"session-store\"]\n");
        let y = render(&s, Topology::Linked, ApiVersion::V1, &Target::default(), Some(&graph()))
            .unwrap();
        assert!(y.contains("bucket: record-store"), "{y}");
        assert!(y.contains("bucket: session-store"), "{y}");
    }

    #[test]
    fn the_rendered_api_version_is_stamped_because_wadm_will_not_complain() {
        // wadm 0.21 silently accepts a v2 manifest with no scalers, so the file must say its version.
        let s = spec("components = [\"gate-domain\", \"record-store\"]\n");
        let t = Target::default();
        let v1 = render(&s, Topology::Fused, ApiVersion::V1, &t, None).unwrap();
        let v2 = render(&s, Topology::Fused, ApiVersion::V2, &t, None).unwrap();
        assert!(v1.contains("holon.dev/api: \"V1\""), "{v1}");
        assert!(v2.contains("holon.dev/api: \"V2\""), "{v2}");
        assert!(v1.contains("holon.dev/topology: \"Fused\""), "{v1}");
    }

    #[test]
    fn v1_and_v2_are_different_documents_not_two_envelopes() {
        let s = spec("components = [\"gate-domain\", \"record-store\"]\n");
        let t = Target::default();

        let v1 = render(&s, Topology::Linked, ApiVersion::V1, &t, Some(&graph())).unwrap();
        assert!(v1.contains("apiVersion: core.oam.dev/v1beta1"), "{v1}");
        assert!(v1.contains("kind: Application"), "{v1}");
        assert!(v1.contains("type: spreadscaler"), "{v1}");

        let v2 = render_workload(&s, "wasmcloud-v2", &t, Some(&graph()), false).unwrap();
        assert!(v2.contains("apiVersion: runtime.wasmcloud.dev/v1alpha1"), "{v2}");
        assert!(v2.contains("kind: Workload"), "{v2}");
        assert!(!v2.contains("spreadscaler") && !v2.contains("traits"), "{v2}");
    }

    #[test]
    fn a_v2_workload_carries_what_the_host_refused_without() {
        let s = spec(
            "components = [\"gate-domain\", \"record-store\"]\n[config]\ngrace-period-secs = \"5\"\n",
        );
        let v2 =
            render_workload(&s, "wasmcloud-v2", &Target::default(), Some(&graph()), true).unwrap();

        assert!(!v2.contains("oci://"), "{v2}");
        assert!(v2.contains("host: gate.example.com"), "{v2}");
        assert!(v2.contains("package: keyvalue"), "{v2}");
        assert!(v2.contains("package: config"), "{v2}");
        assert!(v2.contains("grace-period-secs"), "{v2}");
    }

    #[test]
    fn wasi_config_is_declared_from_the_artifact_not_the_spec_table() {
        let s = spec("components = [\"flags-domain\"]\n");
        let with_import = render_workload(&s, "ns", &Target::default(), None, true).unwrap();
        assert!(with_import.contains("package: config"), "{with_import}");

        let without_import = render_workload(&s, "ns", &Target::default(), None, false).unwrap();
        assert!(!without_import.contains("package: config"), "{without_import}");
    }

    #[test]
    fn a_daemon_gets_its_own_deployment_and_a_rewritten_config_url() {
        let s = spec(
            "components = [\"fs-watcher-domain\"]\n\
             [config]\n\
             fswatch-url = \"http://127.0.0.1:8000\"\n\
             [[daemon]]\n\
             name = \"fswatch\"\n\
             addr = \"127.0.0.1:8000\"\n\
             allow_flag = \"allow-path\"\n\
             allow = [\"/var/log\"]\n",
        );
        let y = render_workload(&s, "ns", &Target::default(), None, true).unwrap();

        assert!(!y.contains("127.0.0.1:8000"), "{y}");
        assert!(
            y.contains("fswatch-url: \"http://fswatch-daemon.ns.svc.cluster.local:8000\""),
            "{y}"
        );

        assert!(y.contains("localResources:\n        allowedHosts:\n          - fswatch-daemon.ns.svc.cluster.local:8000"), "{y}");
        assert!(
            y.contains("allowedIpNameLookups:\n          - fswatch-daemon.ns.svc.cluster.local"),
            "{y}"
        );

        assert!(y.contains("kind: Deployment"), "{y}");
        assert!(y.contains("name: fswatch-daemon"), "{y}");
        assert!(
            y.contains("image: registry.wasmcloud.svc.cluster.local:5000/comp-fswatch:latest"),
            "{y}"
        );
        assert!(y.contains("- --allow-path\n            - \"/var/log\""), "{y}");
        assert!(y.contains("kind: Service"), "{y}");
        assert!(y.contains("port: 8000"), "{y}");
    }

    #[test]
    fn a_worker_gets_a_deployment_and_no_service_and_no_egress() {
        let s = spec(
            "components = [\"park-gateway\"]\n\
             [config]\n\
             park-url = \"http://127.0.0.1:8015\"\n\
             [[daemon]]\n\
             name = \"park\"\n\
             addr = \"127.0.0.1:8015\"\n\
             [[daemon]]\n\
             name = \"resume\"\n\
             extra_args = [\"--sink-url http://sink.ns.svc:8016/park/deliver\"]\n",
        );
        let y = render_workload(&s, "ns", &Target::default(), None, true).unwrap();

        assert!(
            y.contains("allowedHosts:\n          - park-daemon.ns.svc.cluster.local:8015\n"),
            "{y}"
        );
        assert!(!y.contains("resume-daemon.ns.svc"), "no grant to reach a worker: {y}");
        assert!(y.contains("park-url: \"http://park-daemon.ns.svc.cluster.local:8015\""), "{y}");

        assert!(y.contains("name: resume-daemon"), "{y}");
        assert!(
            y.contains("image: registry.wasmcloud.svc.cluster.local:5000/comp-resume:latest"),
            "{y}"
        );
        assert!(
            y.contains("- \"--sink-url\"")
                && y.contains("- \"http://sink.ns.svc:8016/park/deliver\""),
            "{y}"
        );
        let resume = y.split("---\n").find(|d| d.contains("name: resume-daemon")).unwrap();
        assert!(!resume.contains("--addr") && !resume.contains("kind: Service"), "{resume}");
        assert_eq!(y.matches("kind: Service").count(), 1, "{y}");
        assert_eq!(y.matches("kind: Deployment").count(), 2, "{y}");
    }

    #[test]
    fn the_root_component_comes_from_the_spec_rather_than_a_guessed_suffix() {
        let named = spec("components = [\"lan-scanner\"]\n");
        let v2 = render_workload(&named, "ns", &Target::default(), None, false).unwrap();
        assert!(v2.contains("- name: lan-scanner\n"), "{v2}");

        let bare = spec("");
        let v2b = render_workload(&bare, "ns", &Target::default(), None, false).unwrap();
        assert!(v2b.contains("- name: gate-domain\n"), "{v2b}");
    }

    #[test]
    fn a_graph_too_big_to_fuse_is_refused_here_rather_than_on_the_cluster() {
        let many: Vec<String> = (0..31).map(|i| format!("\"cap-{i}\"")).collect();
        let s = spec(&format!("components = [{}]\n", many.join(", ")));
        let err = check_fusable(&s, Topology::Fused).unwrap_err().to_string();
        assert!(err.contains("nested instances"), "{err}");
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
        let h = render_host_config("holon", "prod", "1.6.0", &Target::default());
        assert!(h.contains("k8s.wasmcloud.dev/v1alpha1"), "{h}");
        assert!(h.contains("ADR-0021"), "{h}");
        assert!(h.contains("lattice: prod"), "{h}");
    }
}
