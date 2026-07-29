//! `applier` — the platform's only holder of a Kubernetes credential.
//!
//! `platform-domain` (wasm) decides everything and renders the manifests; this
//! applies them. It exists for one reason: a wasm component cannot talk to the API
//! server, because `wasi:http` validates TLS against webpki roots and the API
//! server presents a cluster-CA certificate. See docs/adr/0003.
//!
//! It holds no business logic, no database and no user concept, so it stays small
//! enough to audit in one sitting — which matters, because it is the process with
//! the dangerous permission.
//!
//! **It does not trust its caller.** Every request names a namespace, and every
//! object in the payload must belong to that namespace, be of an allow-listed
//! kind, and carry no field we have not seen work. A bug on the wasm side
//! therefore cannot become a cross-tenant write.
//!
//! Modes:
//!   --validate-only   never builds a client; validates and reports (CI, tests)
//!   --dry-run         applies with dryRun=All against a real cluster
//!   (default)         server-side apply, field manager `platform`

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use kube::api::{Api, DeleteParams, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams};
use kube::core::ApiResource;
use kube::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Kinds the platform is allowed to create. Anything else is refused, however it
/// got into the payload.
const ALLOWED_KINDS: &[(&str, &str)] = &[
    ("runtime.wasmcloud.dev/v1alpha1", "WorkloadDeployment"),
    ("v1", "Service"),
    ("v1", "Namespace"),
    ("v1", "ResourceQuota"),
    ("networking.k8s.io/v1", "NetworkPolicy"),
    // ADR-0014: an application owns a host, so the platform renders a host pod and
    // the volume its private data NATS stores to. See `check_pod_spec` — a Deployment
    // is by far the most dangerous kind on this list, because it runs images.
    ("apps/v1", "Deployment"),
    ("v1", "PersistentVolumeClaim"),
];

/// Fields we have not seen work on this cluster (used once anywhere, or only in a
/// comment). The renderer never emits them; this makes that a hard boundary rather
/// than a convention. See docs/adr/0003 and 0010.
const FORBIDDEN_KEYS: &[&str] = &["hostSelector", "configFrom", "secretFrom", "tun"];

#[derive(Parser, Clone)]
#[command(name = "applier", about = "Applies platform-rendered manifests to Kubernetes")]
struct Args {
    /// Listen address for the apply API.
    #[arg(long, default_value = "127.0.0.1:8088")]
    addr: String,

    /// Shared secret the caller must present as `x-platform-secret`.
    #[arg(long, env = "APPLIER_SECRET")]
    secret: String,

    /// Validate and report without building a Kubernetes client at all.
    #[arg(long)]
    validate_only: bool,

    /// Apply with dryRun=All (needs a cluster, changes nothing).
    #[arg(long)]
    dry_run: bool,

    /// Poll this platform for current revisions and re-apply them. Omit to disable.
    /// This is ADR-0004's drift correction: the platform has no scheduler, so the
    /// applier pulls.
    #[arg(long)]
    platform_url: Option<String>,

    /// Seconds between re-apply passes.
    #[arg(long, default_value = "300")]
    reapply_interval: u64,

    /// Only ever touch namespaces with this prefix. A second belt on top of the
    /// per-request namespace check.
    #[arg(long, default_value = "tenant-")]
    namespace_prefix: String,

    /// The ONLY images a rendered pod may run (ADR-0014's host pod and its data-NATS
    /// sidecar). Independently configured here rather than read from the manifest:
    /// this is what keeps "apply a Deployment" from meaning "run anything".
    #[arg(long, default_value = "ghcr.io/wasmcloud/wash:2.5.2")]
    host_image: String,

    #[arg(long, default_value = "docker.io/nats:2.12.8-alpine")]
    nats_image: String,

    /// Namespace the runtime-operator runs in. The ONE place outside a tenant
    /// namespace this process touches, and only to delete orphaned `Host` objects
    /// (see `reap_hosts`) — never to create or patch anything.
    #[arg(long, default_value = "platform")]
    operator_namespace: String,

    /// Reserved prefix marking host environments the platform owns. A `Host` whose
    /// environment does not start with this is never considered for deletion, which
    /// is what keeps the chart's own hosts safe.
    #[arg(long, default_value = "app-")]
    env_prefix: String,

    /// Disable orphan reaping entirely.
    #[arg(long)]
    no_reap: bool,

    /// Registry the platform's artifacts are pushed to and pulled from, as
    /// `host:port`. The rendered manifests reference this same host by digest.
    #[arg(long, default_value = "registry.platform.svc.cluster.local:5000")]
    registry: String,

    /// Scheme for talking to the registry. In-cluster registries are plain HTTP —
    /// the host pods pull them with `--allow-insecure-registries`, and the network
    /// path is closed by NetworkPolicy rather than by TLS (ADR-0017).
    #[arg(long, default_value = "http")]
    registry_scheme: String,

    /// Disable pushing entirely.
    #[arg(long)]
    no_push: bool,
}

/// OCI media types, matched to what `wkg oci push` writes — read off a real artifact
/// in the running registry rather than guessed, because the operator has to be able
/// to pull what we produce.
const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MT_CONFIG: &str = "application/vnd.wasm.config.v0+json";
const MT_LAYER: &str = "application/wasm";

/// Delete one application's whole footprint. The platform sends the env; every
/// object the renderer emits carries it as `platform.comp/env`, so this needs no
/// list of names — which matters because a list the platform got wrong would leave
/// objects behind forever.
#[derive(Deserialize)]
struct PruneRequest {
    namespace: String,
    env: String,
}

#[derive(Serialize)]
struct PruneReport {
    namespace: String,
    env: String,
    deleted: Vec<String>,
    dry_run: bool,
    validated_only: bool,
}

#[derive(Deserialize)]
struct ApplyRequest {
    namespace: String,
    /// One or more YAML documents, as the renderer emitted them.
    manifests: String,
}

#[derive(Serialize)]
struct ApplyReport {
    namespace: String,
    applied: Vec<String>,
    dry_run: bool,
    validated_only: bool,
}

struct AppState {
    args: Args,
    client: Option<Client>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.secret.trim().is_empty() {
        bail!("--secret must not be empty: it is the only thing standing between this process's credential and the network");
    }

    let client = if args.validate_only {
        eprintln!("applier: validate-only — no Kubernetes client will be built");
        None
    } else {
        Some(Client::try_default().await.context("building a Kubernetes client (is a kubeconfig or a ServiceAccount present?)")?)
    };

    let state = Arc::new(AppState { args: args.clone(), client });

    if let Some(url) = args.platform_url.clone() {
        let bg = state.clone();
        tokio::spawn(async move { reapply_loop(bg, url).await });
    }

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/apply", post(apply_handler))
        .route("/prune", post(prune_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    eprintln!(
        "applier: listening on http://{} | mode = {} | namespace prefix = {:?}",
        args.addr,
        if args.validate_only {
            "validate-only"
        } else if args.dry_run {
            "dry-run"
        } else {
            "apply"
        },
        args.namespace_prefix
    );
    axum::serve(listener, app).await?;
    Ok(())
}

fn authorized(headers: &HeaderMap, want: &str) -> bool {
    headers
        .get("x-platform-secret")
        .and_then(|v| v.to_str().ok())
        .map(|got| got == want)
        .unwrap_or(false)
}

async fn apply_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ApplyRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.args.secret) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "bad or missing x-platform-secret" }))).into_response();
    }
    match apply(&state, &req).await {
        Ok(report) => (StatusCode::OK, Json(json!(report))).into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "rejected", "detail": format!("{e:#}") })),
        )
            .into_response(),
    }
}


async fn prune_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PruneRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.args.secret) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "bad or missing x-platform-secret" }))).into_response();
    }
    match prune(&state, &req).await {
        Ok(report) => (StatusCode::OK, Json(json!(report))).into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "rejected", "detail": format!("{e:#}") })),
        )
            .into_response(),
    }
}

/// Delete one application: every object labelled with its env, plus the `Host` the
/// operator wrote when its pod registered.
///
/// The env is validated the same way a namespace is, and for the same reason — it is
/// a selector and a cross-namespace reach, so a malformed one must be refused rather
/// than interpreted. Two rules do the work:
///
/// * `env` must carry the reserved prefix, so this can never select a host the chart
///   owns; and
/// * `env` must belong to `namespace` (`app-<tenant>-<app>` inside `tenant-<tenant>`),
///   so one tenant's prune cannot name another tenant's app.
async fn prune(state: &AppState, req: &PruneRequest) -> Result<PruneReport> {
    let ns = req.namespace.trim();
    let env = req.env.trim();
    check_namespace(ns, &state.args.namespace_prefix)?;
    check_env(env, ns, &state.args.namespace_prefix, &state.args.env_prefix)?;

    if state.args.validate_only {
        return Ok(PruneReport {
            namespace: ns.to_string(),
            env: env.to_string(),
            deleted: vec![format!("(validate-only) everything labelled platform.comp/env={env}")],
            dry_run: false,
            validated_only: true,
        });
    }
    let client = state.client.as_ref().expect("client present unless validate-only");
    let params = if state.args.dry_run { DeleteParams::default().dry_run() } else { DeleteParams::default() };
    let selector = ListParams::default().labels(&format!("platform.comp/env={env}"));
    let mut deleted = Vec::new();

    for (api_version, kind) in ALLOWED_KINDS {
        // The namespace itself is never pruned: it holds the tenant's other apps, and
        // its quota and NetworkPolicy are the tenant's, not this app's.
        if *kind == "Namespace" || *kind == "ResourceQuota" || *kind == "NetworkPolicy" {
            continue;
        }
        let gvk = parse_gvk(api_version, kind)?;
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);
        let found = api
            .list(&selector)
            .await
            .with_context(|| format!("listing {kind} in {ns}"))?;
        for obj in found {
            let name = obj.metadata.name.clone().unwrap_or_default();
            api.delete(&name, &params)
                .await
                .with_context(|| format!("deleting {kind}/{name} in {ns}"))?;
            deleted.push(format!("{kind}/{name}"));
        }
    }

    deleted.extend(delete_hosts_for(state, &[env.to_string()], false).await?);
    Ok(PruneReport {
        namespace: ns.to_string(),
        env: env.to_string(),
        deleted,
        dry_run: state.args.dry_run,
        validated_only: false,
    })
}

fn check_namespace(ns: &str, prefix: &str) -> Result<()> {
    if !ns.starts_with(prefix) {
        bail!("namespace {ns:?} does not start with {prefix:?} — the applier only writes platform-managed namespaces");
    }
    if ns.is_empty() || !ns.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("namespace {ns:?} is not a DNS label");
    }
    Ok(())
}

/// An env is only prunable if it is the platform's AND it belongs to this namespace.
fn check_env(env: &str, ns: &str, ns_prefix: &str, env_prefix: &str) -> Result<()> {
    if env.is_empty() || env.len() > 63 || !env.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("env {env:?} is not a DNS label");
    }
    if !env.starts_with(env_prefix) {
        bail!("env {env:?} does not start with {env_prefix:?} — only platform-owned host environments can be pruned");
    }
    // `tenant-acme` must own `app-acme-<app>`, so `app-globex-x` is refused here.
    let tenant = ns.strip_prefix(ns_prefix).unwrap_or_default();
    let owner = format!("{env_prefix}{tenant}-");
    if !env.starts_with(&owner) {
        bail!("env {env:?} does not belong to namespace {ns:?} (expected it to start with {owner:?})");
    }
    Ok(())
}

/// Delete `Host` objects for the given environments — or, when `orphans_of` is true,
/// every platform-owned Host whose environment is NOT in the list.
///
/// This is the one place the applier reaches outside a tenant namespace, so it is
/// fenced twice: only in `--operator-namespace`, and only for Hosts whose
/// `spec.environment` carries the reserved prefix. A Host is written by the operator
/// from what a host advertises, so the platform cannot label it and cannot name it
/// (the name is generated) — matching on the environment is the only handle there is.
async fn delete_hosts_for(
    state: &AppState,
    envs: &[String],
    orphans_of: bool,
) -> Result<Vec<String>> {
    if state.args.no_reap {
        return Ok(Vec::new());
    }
    let client = state.client.as_ref().context("no client")?;

    // On a sweep, "not in the live set" is not enough on its own. The live set comes
    // from the platform's revisions, so a Host whose pod is still running but whose
    // revision the platform has lost would look like an orphan — and reaping it would
    // be the wrong answer to a different bug (the platform forgetting a running app).
    //
    // So an orphan needs BOTH: no revision AND no host pod. The second half is a
    // positive liveness check, which also means a reap can never race a host that is
    // starting up.
    let live_pods: BTreeSet<String> = if orphans_of {
        let gvk = GroupVersionKind::gvk("apps", "v1", "Deployment");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::all_with(client.clone(), &ar);
        let params = ListParams::default().labels("platform.comp/managed=true");
        api.list(&params)
            .await
            .context("listing platform host Deployments (needs cluster-wide list on deployments)")?
            .into_iter()
            .filter_map(|d| {
                d.metadata.labels.as_ref().and_then(|l| l.get("platform.comp/env").cloned())
            })
            .collect()
    } else {
        BTreeSet::new()
    };
    let gvk = GroupVersionKind::gvk("runtime.wasmcloud.dev", "v1alpha1", "Host");
    let ar = ApiResource::from_gvk(&gvk);
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), &state.args.operator_namespace, &ar);
    let params = if state.args.dry_run { DeleteParams::default().dry_run() } else { DeleteParams::default() };

    let mut deleted = Vec::new();
    for host in api.list(&ListParams::default()).await.context("listing Hosts")? {
        let env = host
            .data
            .get("environment")
            .and_then(|e| e.as_str())
            .unwrap_or_default()
            .to_string();
        // The fence: anything not ours is invisible to this function.
        if !env.starts_with(&state.args.env_prefix) {
            continue;
        }
        let matches = envs.iter().any(|e| *e == env);
        if matches == orphans_of {
            continue;
        }
        // The second half of the orphan test — see `live_pods` above.
        if orphans_of && live_pods.contains(&env) {
            eprintln!(
                "applier: Host environment={env} has no revision but its pod is still running — \
                 NOT reaping. The platform has lost a deployment record; that is the bug to fix."
            );
            continue;
        }
        let name = host.metadata.name.clone().unwrap_or_default();
        api.delete(&name, &params).await.with_context(|| format!("deleting Host/{name}"))?;
        eprintln!("applier: reaped Host/{name} (environment={env})");
        deleted.push(format!("Host/{name}"));
    }
    Ok(deleted)
}

/// Parse, validate, then (unless validate-only) server-side apply.
async fn apply(state: &AppState, req: &ApplyRequest) -> Result<ApplyReport> {
    let ns = req.namespace.trim();
    check_namespace(ns, &state.args.namespace_prefix)?;

    let objects = parse_objects(&req.manifests)?;
    if objects.is_empty() {
        bail!("no objects in the payload");
    }

    let allowed_images = vec![state.args.host_image.clone(), state.args.nats_image.clone()];
    let mut names = Vec::new();
    for obj in &objects {
        validate(obj, ns, &allowed_images)?;
        names.push(describe(obj));
    }

    if state.args.validate_only {
        return Ok(ApplyReport {
            namespace: ns.to_string(),
            applied: names,
            dry_run: false,
            validated_only: true,
        });
    }

    let client = state.client.as_ref().expect("client present unless validate-only");
    let mut params = PatchParams::apply("platform").force();
    if state.args.dry_run {
        params = params.dry_run();
    }

    for obj in &objects {
        let (api_version, kind) = gvk_of(obj)?;
        let gvk = parse_gvk(&api_version, &kind)?;
        let ar = ApiResource::from_gvk(&gvk);
        let name = obj
            .metadata
            .name
            .clone()
            .context("every object needs metadata.name")?;

        // A Namespace is cluster-scoped; everything else is namespaced into `ns`.
        let api: Api<DynamicObject> = if kind == "Namespace" {
            Api::all_with(client.clone(), &ar)
        } else {
            Api::namespaced_with(client.clone(), ns, &ar)
        };
        api.patch(&name, &params, &Patch::Apply(obj))
            .await
            .with_context(|| format!("applying {kind}/{name} in {ns}"))?;
    }

    Ok(ApplyReport {
        namespace: ns.to_string(),
        applied: names,
        dry_run: state.args.dry_run,
        validated_only: false,
    })
}

fn parse_objects(manifests: &str) -> Result<Vec<DynamicObject>> {
    let mut out = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(manifests) {
        let value = serde_yaml::Value::deserialize(doc).context("parsing a YAML document")?;
        if value.is_null() {
            continue;
        }
        // Round-trip through JSON so the object is exactly what the API server sees.
        let json_value: serde_json::Value =
            serde_json::to_value(&value).context("converting YAML to JSON")?;
        if json_value.get("kind").is_none() {
            continue;
        }
        out.push(serde_json::from_value(json_value).context("not a Kubernetes object")?);
    }
    Ok(out)
}

fn gvk_of(obj: &DynamicObject) -> Result<(String, String)> {
    let t = obj.types.as_ref().context("object has no apiVersion/kind")?;
    Ok((t.api_version.clone(), t.kind.clone()))
}

fn parse_gvk(api_version: &str, kind: &str) -> Result<GroupVersionKind> {
    Ok(match api_version.split_once('/') {
        Some((group, version)) => GroupVersionKind::gvk(group, version, kind),
        None => GroupVersionKind::gvk("", api_version, kind),
    })
}

fn describe(obj: &DynamicObject) -> String {
    let kind = obj.types.as_ref().map(|t| t.kind.clone()).unwrap_or_default();
    let name = obj.metadata.name.clone().unwrap_or_default();
    format!("{kind}/{name}")
}

/// The checks that make this process safe to give a credential to.
fn validate(obj: &DynamicObject, ns: &str, allowed_images: &[String]) -> Result<()> {
    let (api_version, kind) = gvk_of(obj)?;
    if !ALLOWED_KINDS.iter().any(|(av, k)| *av == api_version && *k == kind) {
        bail!("{api_version}/{kind} is not an allow-listed kind");
    }
    let name = obj.metadata.name.as_deref().context("every object needs metadata.name")?;
    if name.is_empty() || name.len() > 63 {
        bail!("{kind}: metadata.name {name:?} is not a DNS label");
    }

    // The check that matters: an object may not name a namespace other than the one
    // the request is for. A Namespace object may only be the tenant's own.
    match kind.as_str() {
        "Namespace" => {
            if name != ns {
                bail!("a Namespace object must be {ns:?}, got {name:?}");
            }
        }
        _ => match obj.metadata.namespace.as_deref() {
            Some(other) if other != ns => {
                bail!("{kind}/{name} is namespaced into {other:?} but the request is for {ns:?}")
            }
            _ => {}
        },
    }

    if kind == "Deployment" {
        check_pod_spec(obj, name, allowed_images)?;
    }

    // Fields we do not trust yet, wherever they appear in the tree.
    let as_json = serde_json::to_string(obj).unwrap_or_default();
    for key in FORBIDDEN_KEYS {
        if as_json.contains(&format!("\"{key}\"")) {
            bail!("{kind}/{name} uses {key:?}, which is not a verified field on this operator");
        }
    }
    Ok(())
}

/// The check that earns `Deployment` its place on the allow-list.
///
/// Every other allowed kind is declarative data. A Deployment **runs images**, so
/// accepting one turns "the platform may apply manifests" into "the platform may
/// execute arbitrary code in this cluster" — and the platform is a wasm component
/// that tenants send HTTP to. One renderer bug away from a container of someone
/// else's choosing, mounting whatever it likes.
///
/// So the applier does not trust the renderer here. It re-derives the only two
/// images a host pod may run from its own flags, and refuses the pod-level fields
/// that turn a container into a node compromise: host namespaces, privilege,
/// `hostPath` volumes, and a service account (which would hand the pod a Kubernetes
/// token — the applier's own credential is the thing this whole split exists to keep
/// away from tenant-reachable code, ADR-0003).
fn check_pod_spec(obj: &DynamicObject, name: &str, allowed_images: &[String]) -> Result<()> {
    let spec = obj.data.get("spec").context("Deployment needs a spec")?;
    let pod = spec
        .get("template")
        .and_then(|t| t.get("spec"))
        .context("Deployment needs spec.template.spec")?;

    for field in ["hostNetwork", "hostPID", "hostIPC", "serviceAccountName", "serviceAccount"] {
        if pod.get(field).is_some() {
            bail!("Deployment/{name} sets {field:?}, which a platform-rendered host pod never does");
        }
    }
    if let Some(vols) = pod.get("volumes").and_then(|v| v.as_array()) {
        for v in vols {
            if v.get("hostPath").is_some() {
                bail!("Deployment/{name} mounts a hostPath, which would escape the pod");
            }
        }
    }

    let mut images = 0usize;
    for key in ["containers", "initContainers"] {
        for c in pod.get(key).and_then(|v| v.as_array()).map(|a| a.as_slice()).unwrap_or(&[]) {
            let image = c.get("image").and_then(|i| i.as_str()).unwrap_or_default();
            if !allowed_images.iter().any(|a| a == image) {
                bail!(
                    "Deployment/{name} runs image {image:?}, which is not one of the platform's host images {allowed_images:?}"
                );
            }
            images += 1;
            let sc = c.get("securityContext");
            let flag = |k: &str| sc.and_then(|s| s.get(k)).and_then(|v| v.as_bool()).unwrap_or(false);
            if flag("privileged") || flag("allowPrivilegeEscalation") {
                bail!("Deployment/{name} asks for privilege on container {image:?}");
            }
        }
    }
    if images == 0 {
        bail!("Deployment/{name} declares no containers");
    }
    Ok(())
}

// ---- the push path (ADR-0017) ----------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Push one component to the registry, by hand, over the OCI distribution API.
///
/// Four calls and no registry crate: start an upload, PUT the layer, PUT the config,
/// PUT the manifest. The reason to write it out rather than take a dependency is that
/// the **media types have to match `wkg` exactly** — they were read off a real
/// artifact in the running registry — and an abstraction that picks them for us is
/// the one thing we do not want here.
///
/// Returns the **manifest** digest. That distinction matters: the renderer pins
/// `repo@sha256:...`, and a pull by digest resolves the manifest, not the layer. Using
/// the wasm's own hash there would produce a reference that never resolves.
async fn push_artifact(
    http: &reqwest::Client,
    base: &str,
    repo: &str,
    wasm: &[u8],
    exports: &[String],
    imports: &[String],
) -> Result<String> {
    let (config_bytes, manifest_bytes, manifest_digest, layer_digest) =
        oci_shape(wasm, exports, imports);
    upload_blob(http, base, repo, wasm, &layer_digest).await?;
    upload_blob(http, base, repo, &config_bytes, &digest_of(&config_bytes)).await?;

    // Tagged with the artifact's own content hash, short. A tag is human convenience
    // only (ADR-0006) — nothing in a rendered manifest ever references one — and a
    // content-derived tag can never change meaning under someone.
    let tag = &layer_digest["sha256:".len()..][..12];
    let res = http
        .put(format!("{base}/v2/{repo}/manifests/{tag}"))
        .header("content-type", MT_MANIFEST)
        .body(manifest_bytes)
        .send()
        .await
        .context("PUT manifest")?;
    if !res.status().is_success() {
        bail!("registry refused the manifest: {} {}", res.status(), res.text().await.unwrap_or_default());
    }
    Ok(manifest_digest)
}

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

/// The bytes an OCI wasm artifact is made of: `(config, manifest, manifest digest,
/// layer digest)`.
///
/// Pure, so the shape can be asserted against a real `wkg`-produced artifact without a
/// registry — which is the test that matters, since a wrong media type produces
/// something the operator cannot pull.
fn oci_shape(
    wasm: &[u8],
    exports: &[String],
    imports: &[String],
) -> (Vec<u8>, Vec<u8>, String, String) {
    let layer_digest = digest_of(wasm);

    // The config `wkg` writes carries the component's own surface. We already know it
    // from upload-time reflection, so nothing here parses wasm.
    // A FIXED timestamp, deliberately. `created` is part of the config blob, so a
    // wall-clock value there would change the config digest, which changes the
    // manifest digest — meaning the same bytes would push to a different reference
    // every time and a re-push would mint a second identity for one artifact. Pinning
    // it makes the whole push a pure function of the component: same bytes, same
    // digest, and a retry is a no-op the registry deduplicates.
    let config = json!({
        "created": "1970-01-01T00:00:00Z",
        "author": null,
        "architecture": "wasm",
        "os": "wasip2",
        "layerDigests": [layer_digest],
        "component": { "exports": exports, "imports": imports, "target": null },
    });
    let config_bytes = serde_json::to_vec(&config).expect("config json");
    let config_digest = digest_of(&config_bytes);

    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": MT_MANIFEST,
        "config": { "mediaType": MT_CONFIG, "digest": config_digest, "size": config_bytes.len() },
        "layers": [{ "mediaType": MT_LAYER, "digest": layer_digest, "size": wasm.len() }],
    });
    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest json");
    let manifest_digest = digest_of(&manifest_bytes);
    (config_bytes, manifest_bytes, manifest_digest, layer_digest)
}

async fn upload_blob(
    http: &reqwest::Client,
    base: &str,
    repo: &str,
    bytes: &[u8],
    digest: &str,
) -> Result<()> {
    // Already there? Blobs are content-addressed, so this is always safe to skip, and
    // it makes a retried push cheap instead of re-sending the whole component.
    let head = http.head(format!("{base}/v2/{repo}/blobs/{digest}")).send().await;
    if let Ok(r) = head {
        if r.status().is_success() {
            return Ok(());
        }
    }
    let start = http
        .post(format!("{base}/v2/{repo}/blobs/uploads/"))
        .header("content-length", "0")
        .send()
        .await
        .context("starting a blob upload")?;
    if !start.status().is_success() {
        bail!("registry refused an upload session: {}", start.status());
    }
    let location = start
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .context("upload session has no Location")?
        .to_string();
    // Location may be absolute or root-relative; both are legal.
    let url = if location.starts_with("http") { location } else { format!("{base}{location}") };
    let sep = if url.contains('?') { '&' } else { '?' };
    let res = http
        .put(format!("{url}{sep}digest={digest}"))
        .header("content-type", "application/octet-stream")
        .body(bytes.to_vec())
        .send()
        .await
        .context("PUT blob")?;
    if !res.status().is_success() {
        bail!("registry refused a blob: {} {}", res.status(), res.text().await.unwrap_or_default());
    }
    Ok(())
}

/// One pass of the push queue: ask what has no registry reference, push it, report the
/// digest back. Everything about it is idempotent — "pending" is derived from the
/// absence of an `oci_ref`, and blob uploads are content-addressed — so a crash
/// anywhere in here costs a repeated push, never a wrong one.
async fn push_pass(state: &Arc<AppState>, http: &reqwest::Client, platform_url: &str) -> usize {
    let base = platform_url.trim_end_matches('/');
    let pending = match http
        .get(format!("{base}/api/internal/pending-pushes"))
        .header("x-platform-secret", &state.args.secret)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
        Ok(r) => {
            eprintln!("applier: pending-pushes got {}", r.status());
            None
        }
        Err(e) => {
            eprintln!("applier: pending-pushes failed: {e}");
            None
        }
    };
    let Some(pending) = pending else { return 0 };
    let list = pending["pending"].as_array().cloned().unwrap_or_default();
    let registry = format!("{}://{}", state.args.registry_scheme, state.args.registry);
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };

    let mut pushed = 0usize;
    for row in list {
        let (Some(key), Some(repo)) = (row["key"].as_str(), row["repo"].as_str()) else { continue };
        let bytes = match http
            .get(format!("{base}/api/internal/artifact?key={key}"))
            .header("x-platform-secret", &state.args.secret)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.bytes().await.ok().map(|b| b.to_vec()),
            Ok(r) => {
                eprintln!("applier: artifact {key} got {}", r.status());
                None
            }
            Err(e) => {
                eprintln!("applier: artifact {key} failed: {e}");
                None
            }
        };
        let Some(bytes) = bytes else { continue };

        // A corruption check on the fetch, not an authenticity check — and a PREFIX
        // comparison, because the catalog's `sha256` is `wit:reflect`'s 12-char display
        // hash (its `hex12`, the convention `tools/gen-catalog.py` uses), not a full
        // digest. 48 bits is plenty to catch a truncated or mangled transfer, which is
        // the failure this guards; the artifact's real identity is the manifest digest
        // this push returns, which is what a deployment pins (ADR-0006).
        //
        // Found by this check firing on its first real run, which is the argument for
        // having written it.
        if let Some(want) = row["sha256"].as_str() {
            let want = want.trim_start_matches("sha256:");
            let got = sha256_hex(&bytes);
            if want.is_empty() || !got.starts_with(want) {
                eprintln!(
                    "applier: {key} does not match the catalog (expected sha256 to start {want}, fetched {got}) — not pushing"
                );
                continue;
            }
        }

        match push_artifact(
            http,
            &registry,
            repo,
            &bytes,
            &strings(&row["exports"]),
            &strings(&row["imports"]),
        )
        .await
        {
            Ok(digest) => {
                let res = http
                    .post(format!("{base}/api/internal/pushed"))
                    .header("x-platform-secret", &state.args.secret)
                    .json(&json!({ "key": key, "digest": digest }))
                    .send()
                    .await;
                match res {
                    Ok(r) if r.status().is_success() => {
                        eprintln!("applier: pushed {repo} {digest}");
                        pushed += 1;
                    }
                    // The push landed but the platform did not record it. Harmless:
                    // the component stays pending and the next pass re-pushes, which
                    // is content-addressed and therefore free.
                    Ok(r) => eprintln!("applier: pushed {repo} but /pushed got {}", r.status()),
                    Err(e) => eprintln!("applier: pushed {repo} but /pushed failed: {e}"),
                }
            }
            Err(e) => eprintln!("applier: pushing {repo} failed: {e:#}"),
        }
    }
    pushed
}

/// ADR-0004's drift correction. The platform has no scheduler (a wasm component
/// has no background), so the applier pulls the current revisions and re-applies
/// them. Every apply is idempotent, so this is safe to run at any time — that
/// property is the whole design.
async fn reapply_loop(state: Arc<AppState>, platform_url: String) {
    let period = std::time::Duration::from_secs(state.args.reapply_interval.max(10));
    let http = reqwest::Client::new();
    loop {
        tokio::time::sleep(period).await;

        // Push before apply, in the same pass. A rendered manifest references an
        // artifact by digest, so a component that is not in the registry yet cannot be
        // deployed at all — pushing first means one pass takes an upload all the way to
        // running, instead of two.
        // Gated by `--no-push` alone, deliberately. `--validate-only` and `--dry-run`
        // are about not writing to KUBERNETES; a registry push is neither a cluster
        // write nor destructive — blobs are content-addressed, so a push is additive
        // and idempotent. Keeping it enabled means the push path can be exercised with
        // no cluster at all, which is how it was first proven.
        if !state.args.no_push {
            let n = push_pass(&state, &http, &platform_url).await;
            if n > 0 {
                eprintln!("applier: pushed {n} artifact(s)");
            }
        }

        let url = format!("{}/api/internal/revisions", platform_url.trim_end_matches('/'));
        let res = http
            .get(&url)
            .header("x-platform-secret", &state.args.secret)
            .send()
            .await;
        let revisions = match res {
            Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
            Ok(r) => {
                eprintln!("applier: re-apply poll got {}", r.status());
                None
            }
            Err(e) => {
                eprintln!("applier: re-apply poll failed: {e}");
                None
            }
        };
        // A failed poll means we know nothing, so we change nothing. That matters far
        // more for reaping than for applying: treating "the poll failed" as "no apps
        // exist" would delete every platform-owned Host on the cluster.
        let Some(revisions) = revisions else { continue };
        let list = revisions["revisions"].as_array().cloned().unwrap_or_default();
        let (mut ok, mut failed) = (0usize, 0usize);
        for rev in list {
            let (Some(ns), Some(manifests)) =
                (rev["namespace"].as_str(), rev["manifests"].as_str())
            else {
                continue;
            };
            let req = ApplyRequest { namespace: ns.to_string(), manifests: manifests.to_string() };
            match apply(&state, &req).await {
                Ok(_) => ok += 1,
                Err(e) => {
                    failed += 1;
                    eprintln!("applier: re-apply of {ns} failed: {e:#}");
                }
            }
        }
        if ok + failed > 0 {
            eprintln!("applier: re-applied {ok} deployment(s), {failed} failed");
        }

        // Reconcile the hosts. A `Host` is self-registered by a running host pod, so
        // deleting an app's pod leaves the object behind — and nothing else reaps it
        // (measured: two survived a namespace deletion, ADR-0015). The live set is
        // whatever the platform says it has; every platform-owned Host outside that
        // set is an orphan, whether it was left by a delete that half-finished, a
        // crash, or a namespace someone removed by hand.
        //
        // Deliberately derived from the revisions rather than from delete calls: a
        // reconciler that only cleans up when told never converges after the one
        // failure that matters.
        if !state.args.no_reap && !state.args.validate_only {
            let live: Vec<String> = revisions["revisions"]
                .as_array()
                .map(|a| {
                    a.iter().filter_map(|r| r["env"].as_str().map(String::from)).collect()
                })
                .unwrap_or_default();
            match delete_hosts_for(&state, &live, true).await {
                Ok(reaped) if !reaped.is_empty() => {
                    eprintln!("applier: reaped {} orphaned host(s)", reaped.len());
                }
                Ok(_) => {}
                Err(e) => eprintln!("applier: host reap failed: {e:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(yaml: &str) -> DynamicObject {
        parse_objects(yaml).expect("parses").pop().expect("one object")
    }

    fn images() -> Vec<String> {
        vec!["ghcr.io/wasmcloud/wash:2.5.2".into(), "docker.io/nats:2.12.8-alpine".into()]
    }

    /// A host pod as the renderer emits it (ADR-0014), used as the base for the
    /// hostile variants below.
    fn host_pod(mutate: &dyn Fn(&mut serde_json::Value)) -> DynamicObject {
        let mut o = obj(HOST_POD);
        mutate(o.data.get_mut("spec").unwrap());
        o
    }

    const HOST_POD: &str = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: acme-api-host
  namespace: tenant-acme
spec:
  replicas: 1
  template:
    spec:
      initContainers:
        - name: data-nats
          image: docker.io/nats:2.12.8-alpine
          restartPolicy: Always
      containers:
        - name: host
          image: ghcr.io/wasmcloud/wash:2.5.2
"#;

    #[test]
    fn accepts_the_platforms_own_host_pod() {
        validate(&obj(HOST_POD), "tenant-acme", &images()).expect("valid");
    }

    #[test]
    fn a_deployment_may_only_run_the_platforms_images() {
        // The whole reason `Deployment` can be on the allow-list. Without this check,
        // a renderer bug (or anything that could influence the renderer) turns the
        // applier's credential into arbitrary code execution in the cluster.
        let hostile = host_pod(&|spec| {
            spec["template"]["spec"]["containers"][0]["image"] = json!("attacker/miner:latest");
        });
        let err = validate(&hostile, "tenant-acme", &images()).unwrap_err().to_string();
        assert!(err.contains("not one of the platform's host images"), "{err}");

        // The sidecar is checked too — it is an initContainer, which is easy to forget.
        let hostile = host_pod(&|spec| {
            spec["template"]["spec"]["initContainers"][0]["image"] = json!("busybox");
        });
        assert!(validate(&hostile, "tenant-acme", &images()).is_err());
    }

    #[test]
    fn a_deployment_may_not_reach_out_of_its_pod() {
        for (field, value) in [
            ("hostNetwork", json!(true)),
            ("hostPID", json!(true)),
            // A token would give the pod the very API access ADR-0003 keeps away
            // from anything tenants can reach.
            ("serviceAccountName", json!("default")),
        ] {
            let hostile = host_pod(&|spec| spec["template"]["spec"][field] = value.clone());
            assert!(
                validate(&hostile, "tenant-acme", &images()).is_err(),
                "{field} must be refused"
            );
        }

        let hostile = host_pod(&|spec| {
            spec["template"]["spec"]["volumes"] =
                json!([{ "name": "root", "hostPath": { "path": "/" } }]);
        });
        let err = validate(&hostile, "tenant-acme", &images()).unwrap_err().to_string();
        assert!(err.contains("hostPath"), "{err}");

        let hostile = host_pod(&|spec| {
            spec["template"]["spec"]["containers"][0]["securityContext"] =
                json!({ "privileged": true });
        });
        assert!(validate(&hostile, "tenant-acme", &images()).is_err());
    }

    const WORKLOAD: &str = r#"
apiVersion: runtime.wasmcloud.dev/v1alpha1
kind: WorkloadDeployment
metadata:
  name: api
  namespace: tenant-acme
spec:
  replicas: 1
"#;

    /// The fences on prune. Every case here is a way a bug or a hostile caller could
    /// delete something that is not theirs — which is a worse failure than a leak,
    /// because it is not recoverable.
    #[test]
    fn prune_only_reaches_the_callers_own_app() {
        let (nsp, envp) = ("tenant-", "app-");

        // The happy path: acme's own app, in acme's own namespace.
        check_env("app-acme-api", "tenant-acme", nsp, envp).expect("own app");

        // A tenant naming another tenant's app.
        let err = check_env("app-globex-api", "tenant-acme", nsp, envp).unwrap_err().to_string();
        assert!(err.contains("does not belong to namespace"), "{err}");

        // The chart's own hosts live in environments with no reserved prefix, and this
        // is the single check that makes them unreachable. `jobs` and `eshop` are real
        // hosts on the dev cluster; deleting one would take down other people's apps.
        for foreign in ["jobs", "eshop", "default", ""] {
            assert!(
                check_env(foreign, "tenant-acme", nsp, envp).is_err(),
                "{foreign:?} must not be prunable"
            );
        }

        // A selector that is not a label at all.
        assert!(check_env("app-acme-api,x=y", "tenant-acme", nsp, envp).is_err());
        assert!(check_env("app-acme-*", "tenant-acme", nsp, envp).is_err());
        // ...and the namespace fence still applies on this path.
        assert!(check_namespace("kube-system", nsp).is_err());
        assert!(check_namespace("tenant-acme", nsp).is_ok());
    }

    /// The artifact shape, asserted against a REAL `wkg oci push` artifact read out of
    /// the running registry. If any of these drift, the operator gets something it
    /// cannot pull — and it would fail at deploy time on someone else's app, not here.
    #[test]
    fn the_oci_shape_matches_what_wkg_writes() {
        let wasm = b"\0asm\x0d\0\0\0 pretend component";
        let (config, manifest, manifest_digest, layer_digest) = oci_shape(
            wasm,
            &["wasi:http/incoming-handler@0.2.0".to_string()],
            &["wasi:keyvalue/store@0.2.0-draft".to_string()],
        );
        let m: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        assert_eq!(m["mediaType"], "application/vnd.oci.image.manifest.v1+json");
        assert_eq!(m["schemaVersion"], 2);
        assert_eq!(m["config"]["mediaType"], "application/vnd.wasm.config.v0+json");
        assert_eq!(m["layers"][0]["mediaType"], "application/wasm");
        assert_eq!(m["layers"][0]["size"], wasm.len());
        assert_eq!(m["layers"][0]["digest"], layer_digest);
        assert_eq!(m["config"]["digest"], digest_of(&config));

        let c: serde_json::Value = serde_json::from_slice(&config).unwrap();
        assert_eq!(c["architecture"], "wasm");
        assert_eq!(c["os"], "wasip2");
        assert_eq!(c["layerDigests"][0], layer_digest);
        assert_eq!(c["component"]["exports"][0], "wasi:http/incoming-handler@0.2.0");
        assert!(c["component"]["target"].is_null());

        // Same bytes, same digest — always. A wall-clock `created` here would mint a
        // new identity for one artifact on every retry.
        let (_, _, again, _) = oci_shape(
            wasm,
            &["wasi:http/incoming-handler@0.2.0".to_string()],
            &["wasi:keyvalue/store@0.2.0-draft".to_string()],
        );
        assert_eq!(manifest_digest, again, "the push must be a pure function of the bytes");

        // The reference the renderer pins is the MANIFEST digest, not the layer's.
        // Getting this wrong yields a reference that never resolves.
        assert_ne!(manifest_digest, layer_digest);
    }

    /// The four-call upload dance, against a registry that records what it is sent.
    /// This is the half that cannot be checked by inspecting JSON: the upload session,
    /// the `?digest=` parameter, and a relative vs absolute `Location`.
    #[tokio::test]
    async fn pushes_blobs_then_the_manifest() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct Seen {
            blobs: Vec<(String, usize)>,
            manifest: Option<(String, String)>,
        }
        static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
        *SEEN.lock().unwrap() = Some(Seen::default());

        // One handler dispatching on method+path, because OCI repo names contain
        // slashes and axum will not take a wildcard mid-route. Closer to a real
        // registry's routing anyway.
        let app = Router::new().fallback(
            |method: axum::http::Method, uri: axum::http::Uri, body: axum::body::Bytes| async move {
                let path = uri.path().to_string();
                let query = uri.query().unwrap_or_default().to_string();
                if method == axum::http::Method::POST && path.ends_with("/blobs/uploads/") {
                    // Relative Location on purpose: both forms are legal and this is
                    // the one a naive client mishandles.
                    return (StatusCode::ACCEPTED, [("location", "/upload/session-1".to_string())]);
                }
                if method == axum::http::Method::PUT && path == "/upload/session-1" {
                    let digest = query
                        .split('&')
                        .filter_map(|kv| kv.split_once('='))
                        .find(|(k, _)| *k == "digest")
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_default();
                    assert!(digest.starts_with("sha256:"), "no digest param in {query:?}");
                    assert_eq!(digest, digest_of(&body), "the digest must describe the bytes");
                    SEEN.lock().unwrap().as_mut().unwrap().blobs.push((digest, body.len()));
                    return (StatusCode::CREATED, [("location", String::new())]);
                }
                if method == axum::http::Method::PUT && path.contains("/manifests/") {
                    let reference =
                        path.rsplit("/manifests/").next().unwrap_or_default().to_string();
                    SEEN.lock().unwrap().as_mut().unwrap().manifest =
                        Some((reference, digest_of(&body)));
                    return (StatusCode::CREATED, [("location", String::new())]);
                }
                // Anything else, including the HEAD cache probe: not found, so every
                // blob is a fresh upload in this test.
                (StatusCode::NOT_FOUND, [("location", String::new())])
            },
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let wasm = b"\0asm\x0d\0\0\0 a component".to_vec();
        let http = reqwest::Client::new();
        let digest = push_artifact(
            &http,
            &format!("http://{addr}"),
            "acme/api",
            &wasm,
            &["wasi:http/incoming-handler@0.2.0".to_string()],
            &[],
        )
        .await
        .expect("push");

        let seen = SEEN.lock().unwrap().take().unwrap();
        assert_eq!(seen.blobs.len(), 2, "the layer and the config: {:?}", seen.blobs);
        assert!(seen.blobs.iter().any(|(_, len)| *len == wasm.len()), "the wasm itself");
        let (reference, put_digest) = seen.manifest.expect("a manifest was PUT");
        // What we return must be the digest of what we actually sent, or the catalog
        // records a reference the registry cannot resolve.
        assert_eq!(digest, put_digest);
        // Tagged by content, so a tag can never change meaning under someone.
        assert_eq!(reference, digest_of(&wasm)["sha256:".len()..][..12]);
    }

    #[test]
    fn accepts_a_rendered_workload() {
        validate(&obj(WORKLOAD), "tenant-acme", &images()).expect("valid");
    }

    #[test]
    fn refuses_an_object_aimed_at_another_namespace() {
        // The check that turns a wasm-side bug into a 422 instead of a breach.
        let err = validate(&obj(WORKLOAD), "tenant-globex", &images()).unwrap_err().to_string();
        assert!(err.contains("namespaced into"), "{err}");
    }

    #[test]
    fn refuses_kinds_outside_the_allow_list() {
        let secret = r#"
apiVersion: v1
kind: Secret
metadata: { name: creds, namespace: tenant-acme }
"#;
        let err = validate(&obj(secret), "tenant-acme", &images()).unwrap_err().to_string();
        assert!(err.contains("not an allow-listed kind"), "{err}");
        // ...including the one that would be a privilege escalation.
        let rb = r#"
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata: { name: pwn }
"#;
        assert!(validate(&obj(rb), "tenant-acme", &images()).is_err());
    }

    #[test]
    fn refuses_unverified_operator_fields() {
        let with_selector = r#"
apiVersion: runtime.wasmcloud.dev/v1alpha1
kind: WorkloadDeployment
metadata: { name: api, namespace: tenant-acme }
spec:
  template:
    spec:
      hostSelector: { hostgroup: default }
"#;
        let err = validate(&obj(with_selector), "tenant-acme", &images()).unwrap_err().to_string();
        assert!(err.contains("hostSelector"), "{err}");
    }

    #[test]
    fn a_namespace_object_must_be_the_tenants_own() {
        let ns = "apiVersion: v1\nkind: Namespace\nmetadata: { name: kube-system }\n";
        assert!(validate(&obj(ns), "tenant-acme", &images()).is_err());
        let own = "apiVersion: v1\nkind: Namespace\nmetadata: { name: tenant-acme }\n";
        validate(&obj(own), "tenant-acme", &images()).expect("its own namespace is fine");
    }

    #[test]
    fn parses_a_multi_document_render() {
        let both = format!("{WORKLOAD}---\napiVersion: v1\nkind: Service\nmetadata:\n  name: api\n  namespace: tenant-acme\n");
        let objs = parse_objects(&both).unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!(describe(&objs[0]), "WorkloadDeployment/api");
        assert_eq!(describe(&objs[1]), "Service/api");
        // Comments and the leading generated-by banner must not break parsing.
        let with_comments = format!("# generated\n{both}");
        assert_eq!(parse_objects(&with_comments).unwrap().len(), 2);
    }

    #[test]
    fn gvk_maps_to_the_right_resource() {
        let gvk = parse_gvk("runtime.wasmcloud.dev/v1alpha1", "WorkloadDeployment").unwrap();
        assert_eq!(gvk.group, "runtime.wasmcloud.dev");
        assert_eq!(gvk.version, "v1alpha1");
        let ar = ApiResource::from_gvk(&gvk);
        assert_eq!(ar.plural, "workloaddeployments", "the operator's plural");
        // core/v1 has an empty group
        let core = parse_gvk("v1", "Service").unwrap();
        assert_eq!((core.group.as_str(), core.version.as_str()), ("", "v1"));
    }
}
