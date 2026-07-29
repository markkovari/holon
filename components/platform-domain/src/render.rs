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
//! **An application owns a host** (ADR-0014). Each deployment renders its own host
//! pod — `wash host` plus a private NATS sidecar — and the workload is pinned to it
//! by `environment`. That is what separates storage, compute and capability
//! implementations per application: the app's keyvalue buckets and messaging
//! subjects live in a NATS nothing else can reach, and its wasmtime engine, core
//! instance budget and HTTP listener are its own. It is also why the renderer can
//! bind `wasi:keyvalue` and `wasmcloud:messaging` at all — on a shared host it could
//! not, because the bucket is named by the guest (measured: ADR-0012).
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
    /// The app's private data-NATS volume — its keyvalue, blobstore and stream
    /// storage, all of it (ADR-0014).
    pub storage: String,
    /// Requests for the app's host pod. Every application is now a pod, so a plan
    /// prices compute directly rather than only as a share of a hostgroup.
    pub host_cpu: String,
    pub host_memory: String,
}

impl Default for Plan {
    fn default() -> Self {
        // Mirrors what the vet-clinic settled on after `poolSize: 48` starved the
        // host (1344 core instances against a 1000 cap).
        Plan {
            replicas: 1,
            pool_size: 8,
            max_invocations: 200,
            egress: Vec::new(),
            storage: "1Gi".into(),
            host_cpu: "100m".into(),
            host_memory: "256Mi".into(),
        }
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
    /// Deployment name, already validated as a DNS label by the caller.
    pub name: &'a str,
    pub strategy: Strategy,
    pub parts: &'a [Part],
    pub plan: &'a Plan,
    /// Cluster hostname the operator routes on (`Host` header, port 9191).
    pub http_host: &'a str,
    /// The platform's control-plane NATS, which the app's host dials to register and
    /// receive scheduling. Shared by every host — it carries no application data.
    pub scheduler_nats: &'a str,
    /// `wash` image for the app's host pod. Pinned by the platform, never tenant
    /// input; the applier independently refuses any other image (ADR-0003).
    pub host_image: &'a str,
    /// NATS image for the app's private data plane sidecar.
    pub nats_image: &'a str,
    /// The platform's own namespace — where the registry lives, which the tenant's
    /// NetworkPolicy must permit egress to.
    pub platform_ns: &'a str,
    /// Where the runtime-operator and its scheduler NATS live. Usually the same as
    /// `platform_ns`, but not necessarily: on this dev cluster the operator is in
    /// `jobs` while the registry is in `platform`, and an app's host pod must reach
    /// BOTH or it never registers and never pulls.
    pub control_plane_ns: &'a str,
    /// Object-count ceiling for the tenant's quota, from their plan.
    pub max_deployments: u32,
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

/// Reserved prefix for every host environment the platform owns. See `env_for`.
pub const ENV_PREFIX: &str = "app-";

/// A tenant's namespace. Derived, never tenant-supplied (ADR-0002).
pub fn namespace_for(tenant: &str) -> String {
    format!("tenant-{}", dns_label(tenant))
}

/// The host environment one application runs on, and the name of everything that
/// backs it (host pod, data NATS, storage claim). Derived from tenant + deployment,
/// never supplied: this string IS the isolation boundary (ADR-0014), so a tenant
/// being able to set it would be a tenant being able to join someone else's host.
pub fn env_for(tenant: &str, name: &str) -> String {
    // The `app-` prefix is load-bearing, not decoration. A `Host` object is written by
    // the operator from what the host advertises, so the platform cannot label it —
    // and the reaper that deletes orphaned Hosts therefore needs a positive marker of
    // "this is one of mine". The prefix is it: the reaper only ever considers Hosts
    // whose environment starts with `app-`, which makes the chart's own hosts
    // (`jobs`, `eshop`, `default`) untouchable by construction rather than by care.
    let e = format!("{}{}-{}", ENV_PREFIX, dns_label(tenant), dns_label(name));
    // A DNS label ceiling, since it names a Deployment and a PVC.
    e.chars().take(53).collect::<String>().trim_matches('-').to_string()
}

/// An application's storage prefix. Belt to the private data NATS's braces: nothing
/// else can reach that NATS, and within it the app is still namespaced (ADR-0008).
pub fn bucket_for(tenant: &str, name: &str) -> String {
    format!("b-{}", env_for(tenant, name))
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
    let env = env_for(input.tenant, input.name);
    let bucket = bucket_for(input.tenant, input.name);
    let serves_http = input.parts.iter().any(|p| p.serves_http);
    let total_modules: u32 = input.parts.iter().map(|p| p.nested_instances.max(1)).sum();
    let pool = safe_pool_size(input.plan.pool_size, total_modules);

    let mut s = String::new();
    s.push_str("# Generated by platform:app — do not edit; the platform re-applies this.\n");
    s.push_str(&format!("# tenant={} strategy={} \n", input.tenant, input.strategy.as_str()));
    // The tenant's namespace and guardrails ride along with every save. They have to:
    // the app's host pod runs in that namespace and cannot register unless the
    // NetworkPolicy there permits the control plane. Re-applying them is idempotent
    // (ADR-0004), so drift heals on the next save or re-apply pass instead of needing
    // a separate provisioning step nobody would remember to run.
    s.push_str(&render_tenant_namespace(
        input.tenant,
        input.max_deployments,
        input.platform_ns,
        input.control_plane_ns,
    ));
    s.push_str("---\n");
    s.push_str(&render_app_host(input, &ns, &env));
    s.push_str("---\n");
    s.push_str("apiVersion: runtime.wasmcloud.dev/v1alpha1\n");
    s.push_str("kind: WorkloadDeployment\n");
    s.push_str("metadata:\n");
    s.push_str(&format!("  name: {}\n", input.name));
    s.push_str(&format!("  namespace: {ns}\n"));
    s.push_str("  labels:\n");
    s.push_str("    platform.comp/managed: \"true\"\n");
    s.push_str(&format!("    platform.comp/tenant: {}\n", dns_label(input.tenant)));
    s.push_str(&format!("    platform.comp/strategy: {}\n", input.strategy.as_str()));
    // Every object of one app carries its env, so deleting an app is one label
    // selector rather than a list of names the platform has to remember correctly.
    s.push_str(&format!("    platform.comp/env: {env}\n"));
    s.push_str("spec:\n");
    s.push_str(&format!("  replicas: {}\n", input.plan.replicas.max(1)));
    s.push_str("  template:\n    spec:\n");
    // Pin the workload to this application's own host. Without it the operator would
    // schedule onto any host in the namespace, which is every other app of this
    // tenant — the whole boundary is this one line plus the pod it names.
    s.push_str(&format!("      environment: {env}\n"));
    if serves_http {
        s.push_str("      kubernetes:\n        service:\n");
        s.push_str(&format!("          name: {}\n", input.name));
    }

    // ---- hostInterfaces: one entry per interface, always -------------------
    let mut wanted: BTreeSet<HostIface> = BTreeSet::new();
    for p in input.parts {
        for h in &p.host_imports {
            // Ambient families are not declared — see OPERATOR_BOUND. Everything the
            // operator does bind is granted: this host serves one application, so
            // there is no one to share a bucket or a subject with (ADR-0014).
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
        s.push_str(&format!("    platform.comp/env: {env}\n"));
        s.push_str("spec:\n  ports:\n");
        s.push_str("    - name: http\n      port: 80\n      targetPort: 9191\n");
    }
    Ok(s)
}

/// One application's host: a `wash host` pod with a **private NATS sidecar**, plus
/// the claim that NATS stores to.
///
/// This is where "separated storage, compute and implementations" actually happens,
/// and it happens because of one fact about the host binary: the control plane and
/// the data plane are different flags.
///
/// * `--scheduler-nats-url` is the operator's bus. It carries scheduling, not
///   application data, so every app's host shares the platform's one NATS.
/// * `--data-nats-url` is what backs `wasi:keyvalue`, `wasi:blobstore` and
///   `wasmcloud:messaging`. Pointing it at `localhost` in this pod means the app's
///   buckets and subjects live in a NATS that has no Service, no other client, and
///   dies with the app. There is nothing to allow-list because there is no one else.
///
/// Compute follows for free: its own wasmtime engine, its own core-instance budget
/// (so one app's `poolSize` cannot starve another's — the vet-clinic failure), and
/// its own `:9191`, which is why each app gets its own endpoint.
///
/// The NATS is a **native sidecar** (`initContainers` + `restartPolicy: Always`), so
/// it is up before the host starts and shuts down after it — ordinary containers give
/// no such ordering, and the host's first `store::open` would race the bus.
fn render_app_host(input: &RenderInput, ns: &str, env: &str) -> String {
    let mut s = String::new();
    s.push_str("# This application's own host: private data NATS, own engine, own :9191.\n");
    s.push_str("apiVersion: v1\nkind: PersistentVolumeClaim\nmetadata:\n");
    s.push_str(&format!("  name: {env}-data\n  namespace: {ns}\n"));
    s.push_str("  labels:\n    platform.comp/managed: \"true\"\n");
    s.push_str(&format!("    platform.comp/env: {env}\n"));
    s.push_str("spec:\n  accessModes: [ReadWriteOnce]\n  resources:\n    requests:\n");
    s.push_str(&format!("      storage: {}\n", input.plan.storage));
    s.push_str("---\n");

    s.push_str("apiVersion: apps/v1\nkind: Deployment\nmetadata:\n");
    s.push_str(&format!("  name: {env}-host\n  namespace: {ns}\n"));
    s.push_str("  labels:\n    platform.comp/managed: \"true\"\n");
    s.push_str(&format!("    platform.comp/tenant: {}\n", dns_label(input.tenant)));
    s.push_str(&format!("    platform.comp/env: {env}\n"));
    s.push_str("spec:\n  replicas: 1\n");
    // The claim is ReadWriteOnce and JetStream is not shared-filesystem safe, so a
    // rolling update must never run two hosts at once. Scaling an app past one host
    // needs a real NATS cluster — see ADR-0014's ceiling.
    s.push_str("  strategy:\n    type: Recreate\n");
    s.push_str(&format!("  selector:\n    matchLabels:\n      platform.comp/env: {env}\n"));
    s.push_str("  template:\n    metadata:\n      labels:\n");
    s.push_str("        platform.comp/managed: \"true\"\n");
    s.push_str(&format!("        platform.comp/env: {env}\n"));
    // The operator's route controller resolves a running host back to a pod through
    // THIS label. Without it the workload runs but gets no endpoints, so its Service
    // answers nothing — "deployed and unreachable", which is the worst failure shape
    // available. Found by deploying for real; no amount of manifest review would have.
    s.push_str(&format!("        wasmcloud.com/hostgroup: {env}\n"));
    s.push_str("    spec:\n");
    s.push_str("      initContainers:\n");
    s.push_str("        - name: data-nats\n");
    s.push_str(&format!("          image: {}\n", input.nats_image));
    s.push_str("          restartPolicy: Always\n");
    // `-a 127.0.0.1` keeps the bus on loopback: even inside this pod it is not
    // reachable from the cluster network, so the app's data plane has no exposed
    // surface at all.
    s.push_str("          args: [\"-js\", \"-sd\", \"/data\", \"-a\", \"127.0.0.1\"]\n");
    // A native sidecar orders *start*, not readiness — without this probe the host
    // would race the bus and crash-loop until NATS happened to be listening.
    //
    // It must be an `exec` probe, not `tcpSocket`. kubelet dials probes at the POD
    // IP, so a tcpSocket probe against a loopback-only bind is refused forever and
    // the pod never leaves PodInitializing (measured: 25 failures, 5 restarts). The
    // fix is to probe from inside the container, where 127.0.0.1 means what we meant.
    s.push_str("          startupProbe:\n");
    s.push_str("            exec:\n");
    s.push_str("              command: [\"nc\", \"-z\", \"127.0.0.1\", \"4222\"]\n");
    s.push_str("            periodSeconds: 1\n            failureThreshold: 30\n");
    s.push_str("          volumeMounts:\n            - name: data\n              mountPath: /data\n");
    s.push_str("          resources:\n            requests:\n              cpu: 50m\n              memory: 64Mi\n");
    s.push_str("      containers:\n");
    s.push_str("        - name: host\n");
    s.push_str(&format!("          image: {}\n", input.host_image));
    s.push_str("          args:\n");
    s.push_str("            - host\n");
    // The pod IP, not the default (the pod NAME). The route controller takes the
    // host's advertised hostname and, if it is an IP, builds the EndpointSlice from it
    // directly; given a name it tries a pod lookup instead. The chart's own hostgroup
    // passes the IP, and matching it is what makes the app's Service resolve.
    s.push_str("            - --host-name=$(WASMCLOUD_HOST_IP)\n");
    s.push_str(&format!("            - --host-group={env}\n"));
    s.push_str(&format!("            - --environment={env}\n"));
    s.push_str(&format!("            - --scheduler-nats-url={}\n", input.scheduler_nats));
    // The whole point: application data never leaves this pod.
    s.push_str("            - --data-nats-url=nats://127.0.0.1:4222\n");
    s.push_str("            - --http-addr=0.0.0.0:9191\n");
    s.push_str("            - --oci-cache-dir=/oci-cache\n");
    s.push_str("            - --allow-insecure-registries\n");
    s.push_str("          env:\n");
    s.push_str("            - name: HOME\n              value: /tmp\n");
    s.push_str("            - name: WASMCLOUD_HOST_IP\n");
    s.push_str("              valueFrom:\n                fieldRef:\n");
    s.push_str("                  fieldPath: status.podIP\n");
    // Per-app engine budget. `safe_pool_size` clamps the workload against this, and
    // now the budget really is the app's own rather than shared with every neighbour.
    s.push_str("            - name: WASMTIME_POOLING_TOTAL_CORE_INSTANCES\n");
    s.push_str("              value: \"8000\"\n");
    s.push_str("          ports:\n            - containerPort: 9191\n");
    s.push_str("          volumeMounts:\n");
    s.push_str("            - name: oci-cache\n              mountPath: /oci-cache\n");
    s.push_str("            - name: tmp\n              mountPath: /tmp\n");
    s.push_str("          resources:\n            requests:\n");
    s.push_str(&format!("              cpu: {}\n", input.plan.host_cpu));
    s.push_str(&format!("              memory: {}\n", input.plan.host_memory));
    s.push_str("      volumes:\n");
    s.push_str(&format!("        - name: data\n          persistentVolumeClaim:\n            claimName: {env}-data\n"));
    s.push_str("        - name: oci-cache\n          emptyDir: {}\n");
    s.push_str("        - name: tmp\n          emptyDir: {}\n");
    s
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
pub fn render_tenant_namespace(
    tenant: &str,
    max_deployments: u32,
    platform_ns: &str,
    control_plane_ns: &str,
) -> String {
    // Both, deduped: the registry and the scheduler NATS may live in different
    // namespaces, and the host pod needs to reach both. Allowing only one of them
    // produces an app that never starts, for a reason nothing in the app explains.
    let mut namespaces = vec![platform_ns];
    if control_plane_ns != platform_ns {
        namespaces.push(control_plane_ns);
    }
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
    // One host pod and one claim per application (ADR-0014), so the object quota has
    // to cover them or it stops counting the thing that actually costs money.
    s.push_str(&format!("    count/deployments.apps: \"{max_deployments}\"\n"));
    s.push_str(&format!("    count/persistentvolumeclaims: \"{max_deployments}\"\n"));
    s.push_str(&format!("    requests.storage: \"{}Gi\"\n", max_deployments * 4));
    s.push_str("---\n");
    // The outer ring of egress control; `allowedHosts` on each workload is the
    // inner one (ADR-0002/0008).
    //
    // This policy became load-bearing with ADR-0014 and was inert before it: the
    // app's host pod now runs HERE, in the tenant's namespace, so a podSelector in
    // this namespace finally selects it. When components ran on a shared host in
    // another namespace, this object selected nothing at all (found the hard way,
    // ADR-0012).
    //
    // Which means it must now allow what the host itself needs, or the app never
    // registers: the platform's control-plane NATS and the registry it pulls from.
    // The app's own traffic is still governed by `allowedHosts` per component.
    s.push_str("apiVersion: networking.k8s.io/v1\nkind: NetworkPolicy\nmetadata:\n");
    s.push_str(&format!("  name: default-deny-egress\n  namespace: {ns}\n"));
    s.push_str("spec:\n  podSelector: {}\n  policyTypes: [Egress]\n");
    s.push_str("  egress:\n");
    s.push_str("    # DNS only; everything else must go through a workload allow-list.\n");
    s.push_str("    - ports:\n        - protocol: UDP\n          port: 53\n");
    s.push_str("    # The host's control plane and image source. Not application data:\n");
    s.push_str("    # that stays on the NATS sidecar inside the app's own pod.\n");
    s.push_str("    - to:\n");
    for ns in namespaces {
        s.push_str("        - namespaceSelector:\n            matchLabels:\n");
        s.push_str(&format!("              kubernetes.io/metadata.name: {ns}\n"));
    }
    s.push_str("      ports:\n        - protocol: TCP\n          port: 4222\n");
    s.push_str("        - protocol: TCP\n          port: 5000\n");
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
            scheduler_nats: "nats://nats.platform.svc.cluster.local:4222",
            host_image: "ghcr.io/wasmcloud/wash:2.5.2",
            nats_image: "docker.io/nats:2.12.8-alpine",
            platform_ns: "platform",
            control_plane_ns: "platform",
            max_deployments: 5,
        }
    }

    #[test]
    fn every_operator_bound_family_is_granted_because_the_app_owns_its_host() {
        // The inverse of what ADR-0013 had to do. keyvalue and messaging are grantable
        // again — not because the host learned to partition them, but because there is
        // no second app on this host to partition them from (ADR-0014).
        let parts = vec![part(
            "api",
            true,
            vec![
                iface("wasi", "keyvalue", "store"),
                iface("wasmcloud", "messaging", "handler"),
                iface("wasi", "config", "store"),
                iface("wasi", "blobstore", "container"),
            ],
        )];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();

        assert!(out.contains("package: keyvalue"), "{out}");
        assert!(out.contains("package: messaging"), "{out}");
        assert!(out.contains("package: config"));
        assert!(out.contains("package: blobstore"));
        assert!(out.contains("package: http"));
        assert!(out.contains("buckets: b-app-acme-api"), "per-app, not per-tenant: {out}");
    }

    #[test]
    fn the_app_gets_its_own_host_with_a_private_data_nats() {
        // The load-bearing test for ADR-0014. Every clause here is a way the
        // separation could silently not happen.
        let parts = vec![part("api", true, vec![iface("wasi", "keyvalue", "store")])];
        let plan = Plan::default();
        let out = render(&input(&parts, Strategy::Fused, &plan)).unwrap();

        // A host pod of its own, named for the app, in the TENANT's namespace — which
        // is also what makes the namespace NetworkPolicy apply to it at last.
        assert!(out.contains("kind: Deployment"), "{out}");
        assert!(out.contains("name: app-acme-api-host"), "{out}");
        // Every namespaced object lands in the tenant's namespace and nowhere else.
        // `\n  namespace: ` is the metadata form; hostInterfaces entries use
        // `- namespace: wasi` at a deeper indent and must not be caught here.
        assert_eq!(
            out.matches("\n  namespace: ").count(),
            out.matches("\n  namespace: tenant-acme\n").count(),
            "an object escaped the tenant namespace: {out}"
        );
        assert!(out.matches("\n  namespace: tenant-acme\n").count() >= 5, "{out}");

        // Storage: the data plane is a sidecar on loopback. If this ever pointed at a
        // Service, every app on that NATS would share buckets again — the ADR-0012
        // failure, reintroduced by one URL.
        assert!(out.contains("--data-nats-url=nats://127.0.0.1:4222"), "{out}");
        assert!(out.contains("-a\", \"127.0.0.1"), "the NATS must not listen off-pod: {out}");
        assert!(out.contains("--scheduler-nats-url=nats://nats.platform.svc.cluster.local:4222"));
        // ...and it must be a native sidecar, or the host races the bus on startup.
        assert!(out.contains("initContainers:"), "{out}");
        assert!(out.contains("restartPolicy: Always"), "{out}");
        // Durable: a restart must not lose the app's records.
        assert!(out.contains("kind: PersistentVolumeClaim"));
        assert!(out.contains("name: app-acme-api-data"));
        assert!(out.contains("claimName: app-acme-api-data"));
        // JetStream on a ReadWriteOnce claim cannot tolerate two hosts at once.
        assert!(out.contains("type: Recreate"), "{out}");

        // Compute: the workload is pinned to this host and nothing else.
        assert!(out.contains("      environment: app-acme-api\n"), "{out}");
        assert!(out.contains("--environment=app-acme-api"), "{out}");
        assert!(out.contains("--host-group=app-acme-api"), "{out}");

        // Reachability, which is separate from running: the route controller matches a
        // host to a pod by this label and prefers an IP hostname. Missing either leaves
        // the app running with a Service that answers nothing (measured).
        assert!(out.contains("wasmcloud.com/hostgroup: app-acme-api"), "{out}");
        assert!(out.contains("--host-name=$(WASMCLOUD_HOST_IP)"), "{out}");
        assert!(out.contains("fieldPath: status.podIP"), "{out}");
    }

    #[test]
    fn two_apps_of_one_tenant_share_nothing() {
        // The adversarial test ADR-0008 asked for, at the level that failed before:
        // same tenant, same namespace, two apps. Every isolation-bearing name differs.
        let parts = vec![part("api", true, vec![iface("wasi", "keyvalue", "store")])];
        let plan = Plan::default();
        let mut a = input(&parts, Strategy::Fused, &plan);
        a.name = "orders";
        let mut b = input(&parts, Strategy::Fused, &plan);
        b.name = "billing";
        let (a, b) = (render(&a).unwrap(), render(&b).unwrap());

        assert!(a.contains("environment: app-acme-orders") && b.contains("environment: app-acme-billing"));
        assert!(a.contains("app-acme-orders-data") && b.contains("app-acme-billing-data"), "separate claims");
        assert!(a.contains("app-acme-orders-host") && b.contains("app-acme-billing-host"), "separate hosts");
        assert!(!a.contains("billing") && !b.contains("orders"), "no name crosses over");
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
        // one component, digest-pinned. Counted by `poolSize`, which only a
        // component entry carries — the app's host pod has `- name:` lines too.
        assert_eq!(out.matches("          poolSize:").count(), 1);
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

        assert_eq!(out.matches("          poolSize:").count(), 3, "all three components: {out}");
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

        // blobstore containers are allow-listed per APP, not per tenant: two apps of
        // one tenant are two boundaries (ADR-0014), which is the level the cluster
        // test failed at when it was per-tenant (ADR-0012).
        assert!(out.contains("buckets: b-app-acme-api"), "blobstore container allow-list: {out}");
        // keyvalue carries no bucket key, because nothing reads one — the boundary is
        // the private data NATS in the app's own pod, not a manifest field.
        assert!(!out.contains("bucket: b-app-acme-api"), "a key nothing reads is worse than none: {out}");
        assert!(out.contains("interfaces: [store]"), "and it IS bound now: {out}");

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
        assert_eq!(bucket_for("ACME Corp.", "Orders v2"), "b-app-acme-corp-orders-v2");
        assert_eq!(env_for("ACME Corp.", "Orders v2"), "app-acme-corp-orders-v2");
        // A hostile deployment name cannot escape its namespace.
        let parts = vec![part("api", false, vec![])];
        let plan = Plan::default();
        let mut i = input(&parts, Strategy::Fused, &plan);
        i.name = "../../kube-system";
        assert!(matches!(render(&i).unwrap_err(), RenderError::BadName(_)));
    }

    #[test]
    fn tenant_namespace_carries_its_guardrails() {
        let out = render_tenant_namespace("acme", 5, "platform", "jobs");
        assert!(out.contains("kind: Namespace"));
        assert!(out.contains("name: tenant-acme"));
        assert!(out.contains("count/workloaddeployments.runtime.wasmcloud.dev: \"5\""));
        assert!(out.contains("kind: NetworkPolicy"));
        assert!(out.contains("policyTypes: [Egress]"));
        assert!(out.contains("port: 53"), "DNS must survive default-deny: {out}");
        // The app's host pod runs in THIS namespace now, so the policy applies to it
        // — and it must be able to reach the control plane or the app never registers.
        assert!(out.contains("kubernetes.io/metadata.name: platform"), "{out}");
        assert!(out.contains("port: 4222"), "the host's scheduler bus: {out}");
        // Both infra namespaces, because the registry and the operator need not share
        // one — an app whose host cannot reach the scheduler NATS never registers.
        assert!(out.contains("kubernetes.io/metadata.name: jobs"), "{out}");
        assert!(out.contains("count/deployments.apps: \"5\""), "one host pod per app: {out}");
        assert!(out.contains("count/persistentvolumeclaims: \"5\""), "{out}");
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
