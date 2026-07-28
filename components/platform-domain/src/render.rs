//! The renderer: `(graph, strategy, tenant, plan) -> Kubernetes manifests`.
//!
//! A pure function, deliberately. It is the highest-consequence code in the
//! platform — it decides which namespace a workload lands in, which storage
//! buckets it may touch, and where it may dial — so it has no I/O, no clock, and
//! no access to anything it could get wrong asynchronously. Everything it needs
//! arrives as an argument.
//!
//! The output vocabulary is restricted on purpose to the fields that appear in at
//! least two working manifests on this cluster:
//!
//!   replicas, template.spec.kubernetes.service.name,
//!   hostInterfaces[].{namespace,package,interfaces,config},
//!   components[].{name,image,poolSize,maxInvocations,localResources.{allowedHosts,config}}
//!
//! Fields used exactly once anywhere (`hostSelector.hostgroup`, `hostInterfaces[].name`)
//! or only in a comment (`configFrom`, `secretFrom`) are NOT emitted — see
//! docs/adr/0003 and 0010. The applier rejects them too, so a future mistake here
//! fails closed.
//!
//! Two rules encode expensive lessons and are asserted by the tests below:
//!
//! 1. **One `hostInterfaces` entry per interface.** An entry binds to a component
//!    only if that component's world covers EVERY interface listed, so a merged
//!    `[store, atomics]` entry silently skips components importing only `store`.
//! 2. **`allowedHosts` needs both bare and port-qualified forms.** Observed in
//!    `examples/jobs/k8s/jobs.yaml`; egress is fail-closed, so a missing form is a
//!    silent connection refusal at runtime.

use std::collections::BTreeSet;

/// Host interface families the operator actually BINDS via `hostInterfaces`.
///
/// Everything else a component imports from `wasi:*` — `cli`, `io`, `clocks`,
/// `random`, `filesystem` — is ambient: the host provides it without being asked,
/// and no working manifest on this cluster declares it. Emitting those would ask
/// the operator to bind interfaces it has no backend for.
///
/// This matters more since the move to `wasm32-wasip2`: Rust's wasip2 std wires up
/// the whole `wasi:cli` surface (five `terminal-*` interfaces included), so an
/// unfiltered render asks for ~10 bindings that do not exist.
/// Per family, the interfaces the operator actually binds — taken from every
/// `interfaces:` list in the working v2 manifests, not from what a component
/// happens to import. `wasi:http/types` is the trap: a component imports it (it is
/// where the request/response types live) but no manifest ever declares it, because
/// it is type-only and has no backend to bind. `wasi:blobstore` DOES list `types`,
/// which is why this is per-family rather than one global rule.
const OPERATOR_BOUND: &[(&str, &str, &[&str])] = &[
    ("wasi", "http", &["incoming-handler", "outgoing-handler"]),
    ("wasi", "keyvalue", &["store", "atomics", "batch"]),
    ("wasi", "config", &["store"]),
    ("wasi", "blobstore", &["blobstore", "container", "types"]),
    ("wasmcloud", "messaging", &["handler", "consumer", "producer"]),
];

fn operator_binds(h: &HostIface) -> bool {
    OPERATOR_BOUND.iter().any(|(ns, pkg, ifaces)| {
        *ns == h.namespace && *pkg == h.pkg && ifaces.contains(&h.iface.as_str())
    })
}

/// What a tenant's plan permits. Stamped by the platform, never tenant-authored.
#[derive(Clone, Debug)]
pub struct Plan {
    pub replicas: u32,
    /// Warm pre-instantiated instances. Capped against the host engine's
    /// concurrent-core-instance budget — see `safe_pool_size`.
    pub pool_size: u32,
    pub max_invocations: u32,
    /// Destinations this tenant may dial. Rendered as `allowedHosts`, both bare
    /// and port-qualified. Empty means egress is denied entirely.
    pub egress: Vec<String>,
}

impl Default for Plan {
    fn default() -> Self {
        // Mirrors what the vet-clinic settled on after `poolSize: 48` starved the
        // host (1344 core instances against a 1000 cap).
        Plan { replicas: 1, pool_size: 8, max_invocations: 200, egress: Vec::new() }
    }
}

/// How the graph becomes something running (ADR-0005).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// One artifact, composed by `wit:reflect` before deploy.
    Fused,
    /// N components in ONE workload; the runtime links them in-process.
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

/// One component as the renderer needs it: a name, a digest-pinned image, and the
/// host interfaces its surface says it imports.
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    /// MUST be digest-pinned (`repo@sha256:...`) — ADR-0006. `render` refuses a tag.
    pub image: String,
    /// Host imports as `ns:pkg/iface` triples, from `wit:reflect`.
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

pub struct RenderInput<'a> {
    pub tenant: &'a str,
    /// The host ENVIRONMENT to schedule onto. Per the operator's CRD: "Environment,
    /// if set, scopes scheduling to Hosts whose Environment matches this value,
    /// regardless of the Workload's own namespace... only honored when the operator
    /// is started with allowSharedHosts=true".
    ///
    /// This is what makes ADR-0002 (a namespace per tenant) compatible with
    /// PLATFORM.md's shared-hostgroup density bet: the workload object lives in the
    /// tenant's namespace, the component runs on a shared host elsewhere. Platform
    /// infrastructure config, never tenant input. Empty omits the field, in which
    /// case scheduling falls back to hosts in the workload's own namespace.
    pub environment: &'a str,
    /// Deployment name, already validated as a DNS label by the caller.
    pub name: &'a str,
    pub strategy: Strategy,
    pub parts: &'a [Part],
    pub plan: &'a Plan,
    /// Cluster hostname the operator routes on (`Host` header, port 9191).
    pub http_host: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RenderError {
    /// A component whose image is not digest-pinned. ADR-0006 makes this fatal:
    /// re-apply must deploy the same bytes it deployed last time.
    NotDigestPinned(String),
    /// No components.
    Empty,
    /// The deployment or tenant name would not be a legal DNS label.
    BadName(String),
    /// `fused` must arrive as exactly one part — the composed artifact.
    FusedNotComposed(usize),
}

impl RenderError {
    pub fn detail(&self) -> String {
        match self {
            RenderError::NotDigestPinned(c) => format!(
                "component `{c}` has no digest — push it to the registry first (a tag cannot be re-applied reproducibly)"
            ),
            RenderError::Empty => "a deployment needs at least one component".into(),
            RenderError::BadName(n) => format!("`{n}` is not a valid DNS label"),
            RenderError::FusedNotComposed(n) => format!(
                "the fused strategy renders one composed artifact, got {n} parts — compose first"
            ),
        }
    }
}

/// A tenant's namespace. Derived, never tenant-supplied (ADR-0002).
pub fn namespace_for(tenant: &str) -> String {
    format!("tenant-{}", dns_label(tenant))
}

/// A tenant's storage prefix. The isolation stamp keys everything on this
/// (ADR-0008); a tenant never sees or sets it.
pub fn bucket_for(tenant: &str) -> String {
    format!("t-{}", dns_label(tenant))
}

fn dns_label(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    out.trim_matches('-').to_string()
}

fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s == dns_label(s)
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
}

/// `poolSize × modules-per-instance` has to stay under the host engine's
/// concurrent-core-instance cap. The vet-clinic proved the failure: pool 48 over a
/// ~28-module graph starved a 1000-instance host. Clamp rather than trust the plan.
pub fn safe_pool_size(requested: u32, total_modules: u32) -> u32 {
    const CORE_INSTANCE_BUDGET: u32 = 800; // of ~1000, leaving headroom
    let modules = total_modules.max(1);
    let ceiling = (CORE_INSTANCE_BUDGET / modules).max(1);
    requested.clamp(1, ceiling)
}

/// Render the manifest set. Returns YAML documents, joined with `---`.
pub fn render(input: &RenderInput) -> Result<String, RenderError> {
    if input.parts.is_empty() {
        return Err(RenderError::Empty);
    }
    if !is_dns_label(input.name) {
        return Err(RenderError::BadName(input.name.to_string()));
    }
    if input.strategy == Strategy::Fused && input.parts.len() != 1 {
        return Err(RenderError::FusedNotComposed(input.parts.len()));
    }
    for p in input.parts {
        // ADR-0006: digests, never tags.
        if !p.image.contains("@sha256:") {
            return Err(RenderError::NotDigestPinned(p.name.clone()));
        }
        if !is_dns_label(&p.name) {
            return Err(RenderError::BadName(p.name.clone()));
        }
    }

    let ns = namespace_for(input.tenant);
    let bucket = bucket_for(input.tenant);
    let serves_http = input.parts.iter().any(|p| p.serves_http);
    let total_modules: u32 = input.parts.iter().map(|p| p.nested_instances.max(1)).sum();
    let pool = safe_pool_size(input.plan.pool_size, total_modules);

    let mut s = String::new();
    s.push_str("# Generated by platform:app — do not edit; the platform re-applies this.\n");
    s.push_str(&format!("# tenant={} strategy={} \n", input.tenant, input.strategy.as_str()));
    s.push_str("apiVersion: runtime.wasmcloud.dev/v1alpha1\n");
    s.push_str("kind: WorkloadDeployment\n");
    s.push_str("metadata:\n");
    s.push_str(&format!("  name: {}\n", input.name));
    s.push_str(&format!("  namespace: {ns}\n"));
    s.push_str("  labels:\n");
    s.push_str("    platform.comp/managed: \"true\"\n");
    s.push_str(&format!("    platform.comp/tenant: {}\n", dns_label(input.tenant)));
    s.push_str(&format!("    platform.comp/strategy: {}\n", input.strategy.as_str()));
    s.push_str("spec:\n");
    s.push_str(&format!("  replicas: {}\n", input.plan.replicas.max(1)));
    s.push_str("  template:\n    spec:\n");
    if !input.environment.is_empty() {
        s.push_str(&format!("      environment: {}\n", input.environment));
    }
    if serves_http {
        s.push_str("      kubernetes:\n        service:\n");
        s.push_str(&format!("          name: {}\n", input.name));
    }

    // ---- hostInterfaces: one entry per interface, always -------------------
    let mut wanted: BTreeSet<HostIface> = BTreeSet::new();
    for p in input.parts {
        for h in &p.host_imports {
            // Ambient families are not declared — see OPERATOR_BOUND.
            if operator_binds(h) {
                wanted.insert(h.clone());
            }
        }
    }
    if serves_http {
        wanted.insert(HostIface {
            namespace: "wasi".into(),
            pkg: "http".into(),
            iface: "incoming-handler".into(),
        });
    }
    if !wanted.is_empty() {
        s.push_str("      hostInterfaces:\n");
        for h in &wanted {
            s.push_str(&format!("        - namespace: {}\n", h.namespace));
            s.push_str(&format!("          package: {}\n", h.pkg));
            // One interface per entry: an entry binds to a component only if the
            // component's world covers every interface in it.
            s.push_str(&format!("          interfaces: [{}]\n", h.iface));
            match (h.namespace.as_str(), h.pkg.as_str(), h.iface.as_str()) {
                ("wasi", "http", "incoming-handler") => {
                    s.push_str("          config:\n");
                    s.push_str(&format!("            host: {}\n", input.http_host));
                }
                // blobstore DOES allow-list containers per workload — the one
                // storage isolation mechanism with a working precedent.
                ("wasi", "blobstore", _) => {
                    s.push_str("          config:\n");
                    s.push_str(&format!("            buckets: {bucket}\n"));
                }
                // NOT isolated, and saying so beats pretending. The bucket a
                // component gets is the one it passes to `store::open(name)`, matched
                // against a hostInterfaces entry's `name`. Every capability in this
                // catalog hardcodes `open("default")` (records:store:47), so a
                // `config: bucket:` key here is read by nothing — it was stamped for
                // two deploys and isolated nothing, proven by a second tenant reading
                // the first's records. See ADR-0012.
                ("wasi", "keyvalue", _) => {
                    s.push_str("          # NOT tenant-isolated: the component opens\n");
                    s.push_str("          # `default` and the host has one keyvalue backend.\n");
                    s.push_str("          # See docs/adr/0012 — do not add a bucket key here\n");
                    s.push_str("          # expecting it to isolate anything.\n");
                }
                _ => {}
            }
        }
    }

    // ---- components --------------------------------------------------------
    s.push_str("      components:\n");
    for p in input.parts {
        s.push_str(&format!("        - name: {}\n", p.name));
        s.push_str(&format!("          image: {}\n", p.image));
        s.push_str(&format!("          poolSize: {pool}\n"));
        s.push_str(&format!("          maxInvocations: {}\n", input.plan.max_invocations.max(1)));
        s.push_str("          localResources:\n");
        // Egress is fail-closed. An empty list is a deliberate deny-all, not an
        // omission — omitting the key would inherit whatever the host defaults to.
        // It must be `[]`: a key followed by only a comment is YAML null, which is
        // not the same thing and not what the operator would read.
        let hosts = expand_egress(&input.plan.egress);
        if hosts.is_empty() {
            s.push_str("            # this tenant's plan permits no egress\n");
            s.push_str("            allowedHosts: []\n");
            if p.host_imports.iter().any(|h| h.iface == "outgoing-handler") {
                s.push_str("            # NOTE: this component imports wasi:http/outgoing-handler,\n");
                s.push_str("            # so every outbound call it makes will be refused.\n");
            }
        } else {
            s.push_str("            allowedHosts:\n");
            for host in hosts {
                s.push_str(&format!("              - \"{host}\"\n"));
            }
        }
    }

    if serves_http {
        s.push_str("---\n");
        s.push_str("# Selector-less: the operator's route controller maintains the endpoints.\n");
        s.push_str("apiVersion: v1\nkind: Service\nmetadata:\n");
        s.push_str(&format!("  name: {}\n", input.name));
        s.push_str(&format!("  namespace: {ns}\n"));
        s.push_str("  labels:\n    platform.comp/managed: \"true\"\n");
        s.push_str("spec:\n  ports:\n");
        s.push_str("    - name: http\n      port: 80\n      targetPort: 9191\n");
    }
    Ok(s)
}

/// Expand a plan's egress list into `allowedHosts` entries.
///
/// The operator's own schema documents the accepted forms: `*`, `host[:port]`,
/// `scheme://host[:port][/]`, `*.suffix[:port]`, `scheme://*.suffix[:port]`. Two
/// consequences encoded here:
///
/// * A scheme-qualified entry is passed through untouched. Splitting it on `:` to
///   find a port would turn `https://api.example.com` into the host `https` —
///   silently allow-listing the wrong thing, since `https` is itself a legal host.
/// * A bare authority is emitted both bare and port-qualified. `examples/jobs/k8s/jobs.yaml`
///   carries both forms for the same host, and egress is fail-closed, so a missing
///   form is a connection refused at runtime rather than an error at deploy.
///
/// Note the operator treats empty AND absent as deny-all, so `[]` is honest rather
/// than load-bearing; `["*"]` is the documented opt-in for unrestricted egress.
fn expand_egress(egress: &[String]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for e in egress {
        let e = e.trim();
        if e.is_empty() {
            continue;
        }
        // `*` and any scheme-qualified form go through verbatim.
        if e == "*" || e.contains("://") {
            out.insert(e.to_string());
            continue;
        }
        match e.rsplit_once(':') {
            // A trailing `:digits` is a port; keep both forms.
            Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
                out.insert(host.to_string());
                out.insert(e.to_string());
            }
            // Otherwise it is a bare authority (or something with a colon that is
            // not a port, which we do not try to be clever about).
            _ => {
                out.insert(e.to_string());
                out.insert(format!("{e}:80"));
                out.insert(format!("{e}:443"));
            }
        }
    }
    out.into_iter().collect()
}

/// The namespace scaffolding a tenant needs, applied once at tenant creation
/// (ADR-0002). Everything the platform creates for a tenant lives here so that
/// deleting the namespace is a complete teardown.
pub fn render_tenant_namespace(tenant: &str, max_deployments: u32) -> String {
    let ns = namespace_for(tenant);
    let mut s = String::new();
    s.push_str("# Generated by platform:app — the tenant's namespace and its guardrails.\n");
    s.push_str("apiVersion: v1\nkind: Namespace\nmetadata:\n");
    s.push_str(&format!("  name: {ns}\n"));
    s.push_str("  labels:\n    platform.comp/managed: \"true\"\n");
    s.push_str(&format!("    platform.comp/tenant: {}\n", dns_label(tenant)));
    s.push_str("---\n");
    s.push_str("apiVersion: v1\nkind: ResourceQuota\nmetadata:\n");
    s.push_str(&format!("  name: tenant\n  namespace: {ns}\n"));
    s.push_str("spec:\n  hard:\n");
    s.push_str(&format!("    count/workloaddeployments.runtime.wasmcloud.dev: \"{max_deployments}\"\n"));
    s.push_str(&format!("    count/services: \"{}\"\n", max_deployments * 2));
    s.push_str("    count/secrets: \"32\"\n");
    s.push_str("---\n");
    // The outer ring of egress control; `allowedHosts` on each workload is the
    // inner one (ADR-0002/0008).
    s.push_str("apiVersion: networking.k8s.io/v1\nkind: NetworkPolicy\nmetadata:\n");
    s.push_str(&format!("  name: default-deny-egress\n  namespace: {ns}\n"));
    s.push_str("spec:\n  podSelector: {}\n  policyTypes: [Egress]\n");
    s.push_str("  egress:\n");
    s.push_str("    # DNS only; everything else must go through a workload allow-list.\n");
    s.push_str("    - ports:\n        - protocol: UDP\n          port: 53\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(ns: &str, pkg: &str, i: &str) -> HostIface {
        HostIface { namespace: ns.into(), pkg: pkg.into(), iface: i.into() }
    }

    fn part(name: &str, serves_http: bool, host: Vec<HostIface>) -> Part {
        Part {
            name: name.into(),
            image: format!("registry.platform.svc.cluster.local:5000/{name}@sha256:{}", "ab".repeat(32)),
            host_imports: host,
            nested_instances: 4,
            serves_http,
        }
    }

    fn input<'a>(parts: &'a [Part], strategy: Strategy, plan: &'a Plan) -> RenderInput<'a> {
        RenderInput {
            tenant: "acme",
            name: "api",
            strategy,
            parts,
            plan,
            http_host: "api.tenant-acme.svc.cluster.local",
            environment: "",
        }
    }

    #[test]
    fn the_environment_targets_a_shared_host_in_another_namespace() {
        // The workload object lives in the tenant's namespace; the component runs on
        // a shared host whose Environment matches. Without this the operator looks
        // for a host in tenant-<x>, finds none, and the workload never schedules.
        let parts = vec![part("api", true, vec![])];
        let plan = Plan::default();
        let mut i = input(&parts, Strategy::Fused, &plan);
        i.environment = "jobs";
        let out = render(&i).unwrap();
        assert!(out.contains("      environment: jobs\n"), "{out}");
        assert!(out.contains("namespace: tenant-acme"), "the object still belongs to the tenant");
        // Omitted when unset, so a single-namespace deployment behaves as before.
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();
        assert!(!out.contains("environment:"), "{out}");
    }

    #[test]
    fn fused_renders_one_component_and_a_service() {
        let parts = vec![part(
            "api",
            true,
            vec![iface("wasi", "keyvalue", "store"), iface("wasi", "clocks", "wall-clock")],
        )];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();

        assert!(out.contains("kind: WorkloadDeployment"));
        assert!(out.contains("namespace: tenant-acme"), "{out}");
        assert!(out.contains("platform.comp/strategy: fused"));
        // one component, digest-pinned
        assert_eq!(out.matches("        - name: ").count(), 1);
        assert!(out.contains("@sha256:"));
        // ...and a Service, because it serves http
        assert!(out.contains("kind: Service"));
        assert!(out.contains("targetPort: 9191"));
    }

    #[test]
    fn one_host_interface_entry_per_interface() {
        // THE rule: a merged entry binds to nothing that imports only one of them.
        let parts = vec![part(
            "api",
            false,
            vec![
                iface("wasi", "keyvalue", "store"),
                iface("wasi", "keyvalue", "atomics"),
                iface("wasi", "keyvalue", "batch"),
            ],
        )];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();

        assert!(out.contains("interfaces: [store]"), "{out}");
        assert!(out.contains("interfaces: [atomics]"));
        assert!(out.contains("interfaces: [batch]"));
        assert!(!out.contains("interfaces: [store, atomics]"), "never merged: {out}");
        assert_eq!(out.matches("- namespace: wasi").count(), 3);
    }

    #[test]
    fn linked_keeps_every_component_and_dedupes_host_interfaces() {
        let parts = vec![
            part("api", true, vec![iface("wasi", "keyvalue", "store")]),
            part("records", false, vec![iface("wasi", "keyvalue", "store")]),
            part("breaker", false, vec![]),
        ];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Linked, &plan)).unwrap();

        assert_eq!(out.matches("        - name: ").count(), 3, "all three components: {out}");
        // The union, deduped: two components want keyvalue/store, one entry.
        assert_eq!(out.matches("interfaces: [store]").count(), 1);
        // Composable edges leave no trace — the runtime links them in-process.
        assert!(!out.contains("records:store"), "no edge appears in the manifest: {out}");
        assert!(out.contains("platform.comp/strategy: linked"));
    }

    #[test]
    fn isolation_is_stamped_where_it_actually_works() {
        let parts = vec![part(
            "api",
            false,
            vec![iface("wasi", "keyvalue", "store"), iface("wasi", "blobstore", "container")],
        )];
        let plan = Plan { egress: vec!["api.example.com".into()], ..Plan::default() };
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();

        // blobstore: the one storage mechanism with a working precedent.
        assert!(out.contains("buckets: t-acme"), "blobstore container allow-list: {out}");

        // keyvalue: NOT isolated, and the manifest says so rather than carrying a
        // bucket key nothing reads. Proven on a cluster — a second tenant read the
        // first's records straight through it (ADR-0012).
        assert!(!out.contains("bucket: t-acme"), "must not pretend to isolate kv: {out}");
        assert!(out.contains("NOT tenant-isolated"), "{out}");

        // Egress in both forms — a bare-only list silently fails closed on a port.
        assert!(out.contains("- \"api.example.com\""));
        assert!(out.contains("- \"api.example.com:443\""), "{out}");
    }

    #[test]
    fn no_egress_means_an_explicit_empty_list_not_null() {
        let parts = vec![part("api", false, vec![])];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();
        // `allowedHosts:` followed by a comment is YAML null, not an empty list.
        assert!(out.contains("allowedHosts: []"), "must be an explicit []: {out}");
        assert!(!out.contains("allowedHosts:\n            #"), "never null: {out}");
    }

    #[test]
    fn warns_when_a_component_needs_egress_it_cannot_have() {
        // Importing outgoing-handler with an empty allow-list means every outbound
        // call fails closed. Correct, and worth saying out loud.
        let parts = vec![part("api", false, vec![iface("wasi", "http", "outgoing-handler")])];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();
        assert!(out.contains("interfaces: [outgoing-handler]"));
        assert!(out.contains("will be refused"), "{out}");
    }

    #[test]
    fn egress_forms_match_what_the_operator_accepts() {
        // A scheme-qualified entry must survive intact: splitting it on ':' would
        // allow-list the host "https", which is legal-looking and wrong.
        let got = expand_egress(&["https://api.example.com".into()]);
        assert_eq!(got, vec!["https://api.example.com".to_string()], "{got:?}");
        assert!(!got.iter().any(|h| h == "https"), "the scheme is not a host: {got:?}");

        // A bare authority gets both forms, because egress is fail-closed.
        let got = expand_egress(&["db.internal".into()]);
        assert_eq!(got, vec!["db.internal", "db.internal:443", "db.internal:80"]);

        // An explicit port keeps the bare form too (jobs.yaml needs both).
        let got = expand_egress(&["golem-proxy.jobs.svc.cluster.local:9006".into()]);
        assert_eq!(
            got,
            vec![
                "golem-proxy.jobs.svc.cluster.local",
                "golem-proxy.jobs.svc.cluster.local:9006"
            ]
        );

        // The documented wildcard and allow-all forms pass through.
        assert_eq!(expand_egress(&["*".into()]), vec!["*".to_string()]);
        let got = expand_egress(&["*.eshop.svc.cluster.local".into()]);
        assert!(got.contains(&"*.eshop.svc.cluster.local".to_string()), "{got:?}");
    }

    #[test]
    fn type_only_interfaces_are_never_declared() {
        // A component importing wasi:http necessarily imports `types` too; no
        // working manifest declares it, because there is nothing to bind.
        let parts = vec![part(
            "api",
            true,
            vec![
                iface("wasi", "http", "types"),
                iface("wasi", "http", "outgoing-handler"),
                iface("wasi", "keyvalue", "store"),
            ],
        )];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();
        assert!(!out.contains("interfaces: [types]"), "type-only, unbindable: {out}");
        assert!(out.contains("interfaces: [outgoing-handler]"));
        assert!(out.contains("interfaces: [incoming-handler]"), "it serves http");
        assert!(out.contains("interfaces: [store]"));
        assert_eq!(out.matches("- namespace: ").count(), 3, "{out}");
    }

    #[test]
    fn a_tag_is_refused() {
        let mut parts = vec![part("api", false, vec![])];
        parts[0].image = "registry.local:5000/api:0.1.0".into();
        let plan = Plan::default();
        let err = render(&input(&parts, Strategy::Fused, &plan)).unwrap_err();
        assert_eq!(err, RenderError::NotDigestPinned("api".into()));
        assert!(err.detail().contains("push it to the registry first"));
    }

    #[test]
    fn fused_must_arrive_composed() {
        let parts = vec![part("api", false, vec![]), part("records", false, vec![])];
        let plan = Plan::default();
        assert_eq!(
            render(&input(&parts, Strategy::Fused, &plan)).unwrap_err(),
            RenderError::FusedNotComposed(2)
        );
    }

    #[test]
    fn pool_size_is_clamped_against_the_engine_budget() {
        // The vet-clinic's lesson: pool 48 over ~28 modules starved a 1000-instance
        // host. 800/28 = 28, so 48 must come down.
        assert_eq!(safe_pool_size(48, 28), 28);
        assert_eq!(safe_pool_size(8, 4), 8, "a modest ask is untouched");
        assert_eq!(safe_pool_size(1000, 200), 4);
        assert_eq!(safe_pool_size(0, 1), 1, "never zero — zero means instantiate per request");
    }

    #[test]
    fn names_are_derived_never_taken() {
        assert_eq!(namespace_for("ACME Corp."), "tenant-acme-corp");
        assert_eq!(bucket_for("ACME Corp."), "t-acme-corp");
        // A hostile deployment name cannot escape its namespace.
        let parts = vec![part("api", false, vec![])];
        let plan = Plan::default();
        let mut i = input(&parts, Strategy::Fused, &plan);
        i.name = "../../kube-system";
        assert!(matches!(render(&i).unwrap_err(), RenderError::BadName(_)));
    }

    #[test]
    fn tenant_namespace_carries_its_guardrails() {
        let out = render_tenant_namespace("acme", 5);
        assert!(out.contains("kind: Namespace"));
        assert!(out.contains("name: tenant-acme"));
        assert!(out.contains("count/workloaddeployments.runtime.wasmcloud.dev: \"5\""));
        assert!(out.contains("kind: NetworkPolicy"));
        assert!(out.contains("policyTypes: [Egress]"));
        assert!(out.contains("port: 53"), "DNS must survive default-deny: {out}");
    }

    #[test]
    fn ambient_wasi_families_are_not_declared() {
        // What a wasip2 component actually imports: a pile of cli/io/clocks noise
        // plus the one capability the operator can bind.
        let parts = vec![part(
            "api",
            true,
            vec![
                iface("wasi", "keyvalue", "store"),
                iface("wasi", "config", "store"),
                iface("wasi", "cli", "environment"),
                iface("wasi", "cli", "exit"),
                iface("wasi", "cli", "terminal-stdout"),
                iface("wasi", "io", "streams"),
                iface("wasi", "clocks", "wall-clock"),
                iface("wasi", "random", "random"),
                iface("wasi", "filesystem", "preopens"),
            ],
        )];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();

        // Declared: the two the operator binds, plus http because it serves.
        assert!(out.contains("package: keyvalue"), "{out}");
        assert!(out.contains("package: config"));
        assert!(out.contains("package: http"));
        // NOT declared: everything ambient. Asking the operator to bind these is
        // asking for a backend that does not exist.
        for ambient in ["package: cli", "package: io", "package: clocks", "package: random", "package: filesystem"] {
            assert!(!out.contains(ambient), "{ambient} must not be declared:\n{out}");
        }
        assert_eq!(out.matches("- namespace: ").count(), 3, "exactly three entries: {out}");
    }

    /// The vocabulary check: nothing we emit may use a field we have not seen work
    /// in at least two manifests on this cluster (ADR-0003).
    #[test]
    fn emits_only_the_verified_field_vocabulary() {
        let parts = vec![part("api", true, vec![iface("wasi", "keyvalue", "store")])];
        let plan = Plan { egress: vec!["x.local".into()], ..Plan::default() };
        let out = render(&input(&parts, Strategy::Linked, &plan)).unwrap();
        for forbidden in ["hostSelector", "configFrom", "secretFrom", "tun:", "poolSize: 0"] {
            assert!(!out.contains(forbidden), "unverified field {forbidden} in output:\n{out}");
        }
    }
}
